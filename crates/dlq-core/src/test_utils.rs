//! Shared SQS test fixtures, enabled with the `test-utils` feature.
//!
//! Tests run against [ElasticMQ], a lightweight SQS-compatible server. By default each
//! [`SqsTestEnv`] starts its own container (~1s). Set `DLQ_TEST_SQS_ENDPOINT` to point all
//! tests at one already-running server instead (e.g. `docker compose up -d` and
//! `DLQ_TEST_SQS_ENDPOINT=http://localhost:4566`). Every queue gets a unique name, so tests
//! can safely share a server.
//!
//! [ElasticMQ]: https://github.com/softwaremill/elasticmq

use std::time::Duration;

use aws_sdk_sqs::config::Credentials;
use testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use testcontainers_modules::elasticmq::ElasticMq;

/// Environment variable naming an existing SQS endpoint to use instead of starting a container.
pub const SQS_ENDPOINT_ENV: &str = "DLQ_TEST_SQS_ENDPOINT";

const ELASTICMQ_IMAGE: &str = "softwaremill/elasticmq-native";
const ELASTICMQ_TAG: &str = "1.7.1";
const ELASTICMQ_PORT: u16 = 9324;

/// Makes ElasticMQ build queue URLs from the request's Host header, so they point at the
/// mapped host port instead of the in-container default of `localhost:9324`.
const ELASTICMQ_CONF: &str = r#"include classpath("application.conf")
node-address.host = "*"
"#;

/// Builds an AWS config for a local SQS endpoint with static test credentials.
pub fn local_config(endpoint_url: &str) -> aws_config::ConfigLoader {
    aws_config::defaults(aws_config::BehaviorVersion::latest())
        .endpoint_url(endpoint_url)
        .region("us-east-1")
        .credentials_provider(Credentials::new("test", "test", None, None, "static"))
}

/// A local SQS server plus a configured client. A container started by this
/// environment is removed when it is dropped.
pub struct SqsTestEnv {
    endpoint_url: String,
    config: aws_config::SdkConfig,
    client: aws_sdk_sqs::Client,
    _container: Option<ContainerAsync<ElasticMq>>,
}

impl SqsTestEnv {
    /// Uses `DLQ_TEST_SQS_ENDPOINT` if set, otherwise starts an ElasticMQ container.
    pub async fn start() -> Self {
        if let Ok(url) = std::env::var(SQS_ENDPOINT_ENV)
            && !url.is_empty()
        {
            let (config, client) = connect(&url).await;
            wait_until_ready(&client, Duration::from_secs(10))
                .await
                .unwrap_or_else(|e| panic!("SQS endpoint {url} is not reachable: {e:?}"));
            return Self {
                endpoint_url: url,
                config,
                client,
                _container: None,
            };
        }

        // Under heavy parallel container churn, Docker occasionally publishes a port
        // that refuses connections even though the server inside is up. Replace such
        // a container instead of failing the test.
        let mut last_error = None;
        for _ in 0..3 {
            let (endpoint_url, container) = start_container().await;
            let (config, client) = connect(&endpoint_url).await;
            match wait_until_ready(&client, Duration::from_secs(5)).await {
                Ok(()) => {
                    return Self {
                        endpoint_url,
                        config,
                        client,
                        _container: Some(container),
                    };
                }
                Err(e) => last_error = Some(e),
            }
        }
        panic!("ElasticMQ container never became reachable: {last_error:?}");
    }

    pub fn endpoint_url(&self) -> &str {
        &self.endpoint_url
    }

    pub fn config(&self) -> &aws_config::SdkConfig {
        &self.config
    }

    pub fn client(&self) -> &aws_sdk_sqs::Client {
        &self.client
    }

    pub fn dlq(&self) -> crate::DeadLetterQueue {
        crate::DeadLetterQueue::from_config(self.config.clone())
    }

    /// Creates a queue named `{prefix}-{uuid}` and returns its URL.
    pub async fn create_queue(&self, prefix: &str) -> String {
        self.client
            .create_queue()
            .queue_name(unique_queue_name(prefix))
            .send()
            .await
            .expect("create_queue failed")
            .queue_url
            .expect("create_queue returned no QueueUrl")
    }

    /// Receives until `expected` message bodies arrive or a receive comes back empty.
    pub async fn receive_bodies(&self, queue_url: &str, expected: usize) -> Vec<String> {
        let mut bodies = Vec::with_capacity(expected);
        while bodies.len() < expected {
            let output = self
                .client
                .receive_message()
                .queue_url(queue_url)
                .max_number_of_messages(10)
                .wait_time_seconds(1)
                .send()
                .await
                .expect("receive_message failed");
            let messages = output.messages.unwrap_or_default();
            if messages.is_empty() {
                break;
            }
            bodies.extend(messages.into_iter().filter_map(|m| m.body));
        }
        bodies
    }
}

async fn start_container() -> (String, ContainerAsync<ElasticMq>) {
    let container = ElasticMq::default()
        .with_name(ELASTICMQ_IMAGE)
        .with_tag(ELASTICMQ_TAG)
        .with_copy_to("/opt/elasticmq.conf", ELASTICMQ_CONF.as_bytes().to_vec())
        .start()
        .await
        .expect("failed to start ElasticMQ container (is Docker running?)");
    let host = container.get_host().await.expect("container host");
    let port = container
        .get_host_port_ipv4(ELASTICMQ_PORT)
        .await
        .expect("container port");
    (format!("http://{host}:{port}"), container)
}

async fn connect(endpoint_url: &str) -> (aws_config::SdkConfig, aws_sdk_sqs::Client) {
    let config = local_config(endpoint_url).load().await;
    let client = aws_sdk_sqs::Client::new(&config);
    (config, client)
}

/// Retries a cheap call until the endpoint answers or `timeout` passes. The server
/// also logs "started" slightly before Docker's port forwarding accepts connections.
async fn wait_until_ready(client: &aws_sdk_sqs::Client, timeout: Duration) -> crate::Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match client.list_queues().send().await {
            Ok(_) => return Ok(()),
            Err(e) if tokio::time::Instant::now() >= deadline => {
                return Err(crate::Error::ListQueues(Box::new(e)));
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

/// Generates a unique queue name for testing, using a UUID suffix.
pub fn unique_queue_name(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}
