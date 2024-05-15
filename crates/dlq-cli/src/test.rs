use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::process::Command;

#[test]
fn command_does_not_exist() {
    let mut cmd = Command::cargo_bin("dlq").unwrap();

    cmd.arg("something");
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("error: unrecognized subcommand"));

    ()
}

#[test]
fn list_queues() {
    let mut cmd = Command::cargo_bin("dlq").unwrap();

    cmd.arg("list");
    cmd.assert().success().stdout(predicate::str::contains(
        "http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/test",
    ));

    ()
}
