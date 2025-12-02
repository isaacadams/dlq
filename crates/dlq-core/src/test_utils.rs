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

/// A fluent test environment for SQS testing with LocalStack.
/// Automatically cleans up resources when dropped (even on panic).
#[allow(dead_code)]
pub struct TestEnv {
    endpoint_url: String,
    container: ContainerAsync<LocalStack>,
    config: aws_config::SdkConfig,
    client: aws_sdk_sqs::Client,
    debug: bool,
}

#[allow(dead_code)]
impl TestEnv {
    /// Create a new test environment with LocalStack
    pub async fn new(debug: Option<bool>) -> Result<Self, TestcontainersError> {
        let (endpoint_url, container) = localstack().await?;
        let config = local_config(&endpoint_url, None).load().await;
        let client = aws_sdk_sqs::Client::new(&config);

        Ok(Self {
            endpoint_url,
            container: container,
            config,
            client,
            debug: debug.unwrap_or(false),
        })
    }

    /// Create a queue with a unique name (adds UUID suffix)
    pub async fn create_sqs_queue(&self, prefix: &str) -> Result<String, TestcontainersError> {
        create_sqs_queue(&self.container, &prefix, self.debug).await
    }

    /// Create a DeadLetterQueue instance
    pub fn dlq(&self) -> crate::sqs::DeadLetterQueue {
        crate::sqs::DeadLetterQueue {
            config: self.config.clone(),
            client: self.client.clone(),
        }
    }

    /// Get the AWS SDK config
    pub fn config(&self) -> &aws_config::SdkConfig {
        &self.config
    }

    /// Get the SQS client
    pub fn client(&self) -> &aws_sdk_sqs::Client {
        &self.client
    }

    /// Get the endpoint URL
    pub fn endpoint_url(&self) -> &str {
        &self.endpoint_url
    }

    /// Get the full queue URL for a queue name
    pub fn queue_url(&self, queue_name: &str) -> String {
        let endpoint_url = self.endpoint_url.as_str();
        format!("{endpoint_url}/000000000000/{queue_name}")
    }
}

// Container cleanup is handled automatically by testcontainers' Drop implementation
// This Drop impl ensures cleanup happens even if the test panics
// Note: testcontainers will handle the async cleanup in its own Drop handler
impl Drop for TestEnv {
    fn drop(&mut self) {}
}

pub async fn create_sqs_queue<I: Image>(
    container: &ContainerAsync<I>,
    name: &str,
    debug: bool,
) -> Result<String, TestcontainersError> {
    let name = unique_queue_name(name);
    let create_queue_command = testcontainers::core::ExecCommand::new([
        "awslocal",
        "sqs",
        "create-queue",
        "--queue-name",
        &name,
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

    Ok(name)
}

/// Generate a unique queue name for testing, using a UUID suffix.
pub fn unique_queue_name(prefix: &str) -> String {
    format!("{}-{}", prefix, uuid::Uuid::new_v4().simple())
}

/// Set up a DeadLetterQueue instance with a test queue.
/// Returns both the DLQ instance and the queue URL.
/// Retries getting the queue URL to handle eventual consistency.
#[allow(dead_code)]
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

    let dlq = crate::sqs::DeadLetterQueue { config, client };

    (dlq, queue_url)
}
