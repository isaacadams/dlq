use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::process::Command;
use testcontainers::ContainerAsync;
use testcontainers_modules::{
    localstack::LocalStack,
    testcontainers::{runners::AsyncRunner, Image, ImageExt, TestcontainersError},
};
use tokio::io::AsyncReadExt;

async fn localstack() -> Result<(String, ContainerAsync<LocalStack>), TestcontainersError> {
    let request = LocalStack::default().with_env_var("SERVICES", "sqs:4576,s3");
    let container = request.start().await?;

    let host_ip = container.get_host().await?;
    let host_port = container.get_host_port_ipv4(4566).await?;
    let endpoint_url = format!("http://{host_ip}:{host_port}");

    Ok((endpoint_url, container))
}

async fn create_test_queue<I: Image>(
    container: &ContainerAsync<I>,
    name: &str,
    debug: bool,
) -> Result<(), TestcontainersError> {
    let create_queue_command = testcontainers::core::ExecCommand::new([
        "awslocal",
        "sqs",
        "create-queue",
        "--queue-name",
        name,
    ])
    .with_container_ready_conditions(vec![testcontainers::core::WaitFor::message_on_stdout(
        "AWS sqs.CreateQueue => 200",
    )]);

    let mut output = container.exec(create_queue_command).await?;

    if debug {
        let mut stdout = String::new();
        let mut stderr = String::new();
        output.stdout().read_to_string(&mut stdout).await.unwrap();
        output.stderr().read_to_string(&mut stderr).await.unwrap();
        println!(
            "Queue creation command output:\nstdout: {}\nstderr: {}",
            stdout, stderr
        );
    }

    Ok(())
}

fn setup_localstack_env(endpoint_url: &str) {
    std::env::set_var("AWS_ENDPOINT_URL", endpoint_url);
    std::env::set_var("AWS_ACCESS_KEY_ID", "test");
    std::env::set_var("AWS_SECRET_ACCESS_KEY", "test");
    std::env::set_var("AWS_DEFAULT_REGION", "us-east-1");
}

#[tokio::test]
async fn command_does_not_exist() {
    let mut cmd = Command::cargo_bin("dlq").unwrap();

    cmd.arg("something");
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("error: unrecognized subcommand"));

    ()
}

#[tokio::test]
async fn list_queues() {
    let (endpoint, container) = localstack().await.unwrap();

    setup_localstack_env(&endpoint);
    create_test_queue(&container, "test-queue", false)
        .await
        .unwrap();

    let mut cmd = Command::cargo_bin("dlq").unwrap();

    cmd.arg("list");
    cmd.assert().success().stdout(predicate::str::contains(
        "http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/test-queue",
    ));

    container.stop().await.unwrap();

    ()
}

#[tokio::test]
async fn poll_queue() {
    let (endpoint, container) = localstack().await.unwrap();

    setup_localstack_env(&endpoint);
    create_test_queue(&container, "test-queue", false)
        .await
        .unwrap();
    let queue_url =
        format!("http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/test-queue");
    send_messages_to_queue(&queue_url, 11).await.unwrap();

    let mut cmd = Command::cargo_bin("dlq").unwrap();

    cmd.arg("poll");
    cmd.arg(&queue_url);

    cmd.assert().success().stdout(
        predicate::str::contains(r#""body":"Test message 0""#)
            .and(predicate::str::contains(r#""body":"Test message 1""#))
            .and(predicate::str::contains(r#""body":"Test message 2""#))
            .and(predicate::str::contains(r#""body":"Test message 3""#))
            .and(predicate::str::contains(r#""body":"Test message 4""#))
            .and(predicate::str::contains(r#""body":"Test message 5""#))
            .and(predicate::str::contains(r#""body":"Test message 6""#))
            .and(predicate::str::contains(r#""body":"Test message 7""#))
            .and(predicate::str::contains(r#""body":"Test message 8""#))
            .and(predicate::str::contains(r#""body":"Test message 9""#))
            .and(predicate::str::contains(r#""body":"Test message 10""#)),
    );

    container.stop().await.unwrap();

    ()
}

async fn send_messages_to_queue(
    queue_url: &str,
    num_messages: i32,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = aws_config::from_env().load().await;
    let client = aws_sdk_sqs::Client::new(&config);

    for batch in (0..num_messages).collect::<Vec<_>>().chunks(10) {
        let entries: Vec<aws_sdk_sqs::types::SendMessageBatchRequestEntry> = batch
            .iter()
            .map(|i| {
                aws_sdk_sqs::types::SendMessageBatchRequestEntry::builder()
                    .id(format!("msg_{}", i))
                    .message_body(format!("Test message {}", i))
                    .build()
                    .unwrap()
            })
            .collect();

        client
            .send_message_batch()
            .queue_url(queue_url)
            .set_entries(Some(entries))
            .send()
            .await?;
    }

    Ok(())
}
