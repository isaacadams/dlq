//! End-to-end tests for the `dlq` binary.

use assert_cmd::Command;
use dlq::test_utils::SqsTestEnv;
use predicates::prelude::*;

fn dlq_cmd(endpoint: &str) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("dlq"));
    cmd.args(["--local", "--endpoint", endpoint]);
    cmd
}

#[test]
fn command_does_not_exist() {
    Command::new(assert_cmd::cargo::cargo_bin!("dlq"))
        .arg("something")
        .assert()
        .failure()
        .stderr(predicate::str::contains("error: unrecognized subcommand"));
}

#[tokio::test]
async fn list_queues() {
    let env = SqsTestEnv::start().await;
    let queue_url = env.create_queue("test-queue").await;

    dlq_cmd(env.endpoint_url())
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains(queue_url));
}

#[tokio::test]
async fn poll_queue() {
    let env = SqsTestEnv::start().await;
    let queue_url = env.create_queue("test-queue").await;

    let messages: Vec<String> = (0..11).map(|i| format!("Test message {i}")).collect();
    for batch in messages.chunks(10) {
        env.dlq().send_batch(&queue_url, batch).await.unwrap();
    }

    let mut expected = predicate::str::contains(r#""body":"Test message 0""#).boxed();
    for i in 1..11 {
        let body = format!(r#""body":"Test message {i}""#);
        expected = expected.and(predicate::str::contains(body)).boxed();
    }

    dlq_cmd(env.endpoint_url())
        .args(["poll", &queue_url])
        .assert()
        .success()
        .stdout(expected);
}

#[test]
fn unreachable_endpoint_reports_error() {
    // Port 9 (discard) is closed on test machines; the CLI must fail cleanly, not panic.
    dlq_cmd("http://127.0.0.1:9")
        .arg("list")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("failed to list queues"));
}

#[tokio::test]
async fn redrive_moves_messages() {
    let env = SqsTestEnv::start().await;
    let source = env.create_queue("cli-redrive-src").await;
    let destination = env.create_queue("cli-redrive-dst").await;
    env.dlq()
        .send_batch(&source, &["a", "b", "c"])
        .await
        .unwrap();

    dlq_cmd(env.endpoint_url())
        .args(["redrive", &source, &destination])
        .assert()
        .success()
        .stdout(predicate::str::contains("Moved 3 message(s)"));

    assert_eq!(env.receive_bodies(&destination, 3).await.len(), 3);
}

#[tokio::test]
async fn purge_requires_confirmation_when_not_interactive() {
    let env = SqsTestEnv::start().await;
    let queue_url = env.create_queue("cli-purge").await;
    env.dlq()
        .send_batch(&queue_url, &["keep me"])
        .await
        .unwrap();

    // stdin is not a terminal here, so without --yes nothing is deleted.
    dlq_cmd(env.endpoint_url())
        .args(["purge", &queue_url])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("pass --yes"));
    assert_eq!(env.receive_bodies(&queue_url, 1).await, ["keep me"]);
}

#[tokio::test]
async fn purge_with_yes_empties_queue() {
    let env = SqsTestEnv::start().await;
    let queue_url = env.create_queue("cli-purge-yes").await;
    env.dlq().send_batch(&queue_url, &["a", "b"]).await.unwrap();

    dlq_cmd(env.endpoint_url())
        .args(["purge", "--yes", &queue_url])
        .assert()
        .success();
    assert!(env.receive_bodies(&queue_url, 1).await.is_empty());
}

#[test]
fn completions_and_man_page() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        Command::new(assert_cmd::cargo::cargo_bin!("dlq"))
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(predicate::str::contains("redrive"));
    }

    Command::new(assert_cmd::cargo::cargo_bin!("dlq"))
        .arg("man")
        .assert()
        .success()
        .stdout(predicate::str::starts_with(".ie").and(predicate::str::contains("redrive")));
}

#[test]
fn version_flag() {
    Command::new(assert_cmd::cargo::cargo_bin!("dlq"))
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[cfg(unix)]
#[test]
fn closed_stdout_exits_quietly() {
    // `dlq man | head -1`: the reader closes the pipe early.
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "'{}' man | head -1 >/dev/null",
            assert_cmd::cargo::cargo_bin!("dlq").display()
        ))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}
