use assert_cmd::prelude::*;
use aws_sdk_sqs::config::Credentials;
use predicates::prelude::*;
use std::process::Command;
use testcontainers::ContainerAsync;
use testcontainers_modules::{
    localstack::LocalStack,
    testcontainers::{runners::AsyncRunner, Image, ImageExt, TestcontainersError},
};
use tokio::io::AsyncReadExt;

pub async fn localstack() -> Result<(String, ContainerAsync<LocalStack>), TestcontainersError> {
    let request = LocalStack::default()
        .with_tag("latest")
        .with_env_var("SERVICES", "sqs:4576,s3")
        .with_env_var("SKIP_SSL_CERT_DOWNLOAD", "1");
    let container = request.start().await?;

    let host_ip = container.get_host().await?;
    let host_port = container.get_host_port_ipv4(4566).await?;
    let endpoint_url = format!("http://{host_ip}:{host_port}");

    Ok((endpoint_url, container))
}

pub async fn create_test_queue<I: Image>(
    container: &ContainerAsync<I>,
    name: &str,
    debug: bool,
) -> Result<String, TestcontainersError> {
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

    let mut result = container.exec(create_queue_command).await?;

    if debug {
        let mut stdout = String::new();
        let mut stderr = String::new();
        result.stdout().read_to_string(&mut stdout).await.unwrap();
        result.stderr().read_to_string(&mut stderr).await.unwrap();
        println!(
            "Queue creation command output:\nstdout: {}\nstderr: {}",
            stdout, stderr
        );
    }

    let output = result.stdout_to_vec().await?;

    let json: serde_json::Value =
        serde_json::from_slice(&output).map_err(|e| TestcontainersError::Other(Box::new(e)))?;

    match json["QueueUrl"].as_str() {
        Some(url) => Ok(url.to_string()),
        None => Err(TestcontainersError::Other(
            "QueueUrl not found in response".into(),
        )),
    }
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

    create_test_queue(&container, "test-queue", false)
        .await
        .unwrap();

    let mut cmd = Command::cargo_bin("dlq").unwrap();

    cmd.args(["--local", "--endpoint", &endpoint]);
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

    let queue_url = create_test_queue(&container, "test-queue", false)
        .await
        .unwrap();

    send_messages_to_queue(&queue_url, 11, &endpoint)
        .await
        .unwrap();

    let mut cmd = Command::cargo_bin("dlq").unwrap();

    cmd.args(["--local", "--endpoint", &endpoint]);
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

pub fn local_config(endpoint_url: &str, region: Option<&'static str>) -> aws_config::ConfigLoader {
    aws_config::defaults(aws_config::BehaviorVersion::latest())
        .endpoint_url(endpoint_url)
        .region(region.unwrap_or("us-east-1"))
        .credentials_provider(Credentials::new("test", "test", None, None, "static"))
}

async fn send_messages_to_queue(
    queue_url: &str,
    num_messages: i32,
    endpoint_url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = local_config(endpoint_url, None).load().await;
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
