use anyhow::Context;
use serde_json::Value;
use std::process::Command;

pub fn create_or_get_queue_url(queue_name: &str) -> String {
    let output = Command::new("aws")
        .args([
            "--endpoint-url",
            "http://localhost:4566",
            "sqs",
            "create-queue",
            "--queue-name",
            queue_name,
            "--output",
            "json",
        ])
        .output()
        .with_context(|| "AWS CLI failed".to_string())
        .unwrap();

    // aws cli returns 200 even if queue exists → same JSON
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("AWS CLI failed: {stderr}");
    }

    let json: Value = serde_json::from_slice(&output.stdout)
        .with_context(|| "Failed to parse JSON response".to_string())
        .unwrap();

    let queue_url = json["QueueUrl"]
        .as_str()
        .with_context(|| "QueueUrl not found in response".to_string())
        .unwrap()
        .to_string();

    // Purge the queue to remove any leftover messages from previous test runs
    let _ = Command::new("aws")
        .args([
            "--endpoint-url",
            "http://localhost:4566",
            "sqs",
            "purge-queue",
            "--queue-url",
            &queue_url,
        ])
        .output();

    queue_url
}

pub async fn local_aws_config() -> aws_config::SdkConfig {
    aws_config::defaults(aws_config::BehaviorVersion::latest())
        .credentials_provider(aws_sdk_sqs::config::Credentials::new(
            "test", "test", None, None, "static",
        ))
        .endpoint_url("http://localhost:4566")
        .region("us-east-1")
        .load()
        .await
}
