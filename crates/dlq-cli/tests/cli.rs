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
