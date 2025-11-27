use aws_sdk_sqs::config::Credentials;
use testcontainers::ContainerAsync;
use testcontainers_modules::{
    localstack::LocalStack,
    testcontainers::{runners::AsyncRunner, Image, ImageExt, TestcontainersError},
};
use tokio::io::AsyncReadExt;

pub fn local_config(endpoint_url: &str, region: Option<&'static str>) -> aws_config::ConfigLoader {
    aws_config::defaults(aws_config::BehaviorVersion::latest())
        .endpoint_url(endpoint_url)
        .region(region.unwrap_or("us-east-1"))
        .credentials_provider(Credentials::new("test", "test", None, None, "static"))
}

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

/// Generate a unique queue name for testing, using a UUID suffix.
pub fn unique_queue_name(prefix: &str) -> String {
    format!("{}-{}", prefix, uuid::Uuid::new_v4().simple())
}

/// Set up a DeadLetterQueue instance with a test queue.
/// Returns both the DLQ instance and the queue URL.
/// Retries getting the queue URL to handle eventual consistency.
pub async fn setup_dlq_with_queue(
    config: aws_config::SdkConfig,
    queue_name: &str,
) -> (crate::sqs::DeadLetterQueue, String) {
    //let config = local_config(endpoint_url, None).load().await;
    let client = aws_sdk_sqs::Client::new(&config);

    // Retry getting queue URL - LocalStack may need a moment for the queue to be available
    let queue_url = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut retries = 0;
        loop {
            match client.get_queue_url().queue_name(queue_name).send().await {
                Ok(output) => {
                    if let Some(url) = output.queue_url() {
                        return url.to_string();
                    }
                }
                Err(_) => {
                    retries += 1;
                    if retries >= 10 {
                        panic!("Failed to get queue URL after {} retries", retries);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    })
    .await
    .expect("Timeout waiting for queue to be available");

    let dlq = crate::sqs::DeadLetterQueue {
        config,
        client,
        default_queue_url: Some(queue_url.clone()),
    };

    (dlq, queue_url)
}
