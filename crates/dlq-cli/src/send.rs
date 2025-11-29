pub async fn run() {
    let (h_stdin, rx_stdin) = crate::reader::concurrent_lines(tokio::io::stdin(), 100);

    SqsBatch::local(
        "http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/demo",
        Some("http://localhost:4566"),
    )
    .await
    .send(rx_stdin)
    .await;

    if let Err(e) = h_stdin.await {
        eprintln!("error from stdin: {e}");
    }
}

pub struct SqsBatch {
    pub aws_config: aws_config::SdkConfig,
    pub aws_sqs_queue_url: String,
}

impl SqsBatch {
    pub async fn local(aws_sqs_queue_url: &str, aws_endpoint: Option<impl Into<String>>) -> Self {
        let endpoint = aws_endpoint
            .map(|s| s.into())
            .or(std::env::var("AWS_ENDPOINT").ok())
            .or(Some("http://localhost:4566".into()));

        let mut loader = aws_config::from_env()
            .region(
                // supports loading region from known env variables
                aws_config::meta::region::RegionProviderChain::default_provider()
                    .or_else(aws_config::Region::from_static("us-east-1")),
            )
            .credentials_provider(aws_sdk_sqs::config::Credentials::new(
                "test", "test", None, None, "static",
            ));

        if let Some(endpoint) = endpoint {
            loader = loader.endpoint_url(endpoint);
        }

        Self {
            aws_config: loader.load().await,
            aws_sqs_queue_url: aws_sqs_queue_url.to_string(),
        }
    }

    pub async fn send(&self, rx: tokio::sync::mpsc::Receiver<String>) {
        let client = std::sync::Arc::new(aws_sdk_sqs::Client::new(&self.aws_config));
        let aws_sqs_queue_url = std::sync::Arc::new(self.aws_sqs_queue_url.clone());

        use pumps::{Concurrency, Pipeline};
        let (mut rx_pipeline, h_pipeline) = Pipeline::from(rx)
            .filter_map(
                |x| async move {
                    if x.is_empty() {
                        return None;
                    }

                    println!("{}", x);

                    // BatchId is a wrapper around a string that validates the ID is a valid SQS batch ID
                    // BatchId::new(uuid::Uuid::new_v4().to_string()).unwrap().0
                    let id = uuid::Uuid::new_v4().hyphenated().to_string();
                    let entry = aws_sdk_sqs::types::SendMessageBatchRequestEntry::builder()
                        .id(id)
                        .message_body(x)
                        .build()
                        .unwrap();

                    println!("entry: {:#?}", entry);
                    Some(entry)
                },
                Concurrency::serial(),
            )
            .batch(10)
            .map(
                move |entries| {
                    let client = client.clone();
                    let aws_sqs_queue_url = aws_sqs_queue_url.clone();
                    async move {
                        job(&client, aws_sqs_queue_url.as_ref(), entries).await;
                    }
                },
                // SQS batch sends are inherently unordered in results (failed items can come back in any order)
                // using unordered concurrency gives better throughput
                Concurrency::concurrent_unordered(10),
            )
            .build();

        while rx_pipeline.recv().await.is_some() {}

        if let Err(e) = h_pipeline.await {
            eprintln!("error from pipeline: {e}");
        }
    }
}

async fn job(
    client: &aws_sdk_sqs::Client,
    queue_url: &str,
    entries: Vec<aws_sdk_sqs::types::SendMessageBatchRequestEntry>,
) {
    match client
        .send_message_batch()
        .queue_url(queue_url)
        .set_entries(Some(entries))
        .send()
        .await
    {
        Ok(output) => {
            println!("output: {:#?}", output);
        }
        Err(e) => {
            if let aws_sdk_sqs::error::SdkError::ServiceError(se) = &e {
                let err = se.err();
                // This prints the primary AWS service error message (e.g., from the XML/JSON response)
                eprintln!("[AWS SDK ERROR] {}", err);
            } else {
                // Fallback for non-service errors (e.g., timeout, dispatch failure)
                eprintln!("[AWS SDK ERROR] {}", e);
            }
        }
    }
}

#[allow(dead_code)]
mod validation {
    use std::convert::Into;

    #[derive(Debug, Clone)]
    pub struct BatchId(String);

    impl BatchId {
        pub fn new<S: Into<String>>(id: S) -> Result<Self, String> {
            let id_str = id.into();
            if id_str.is_empty() {
                return Err("Batch ID cannot be empty".to_string());
            }
            if id_str.len() > 80 {
                return Err(format!(
                    "Batch ID exceeds maximum length: {} > 80 characters",
                    id_str.len()
                ));
            }
            for c in id_str.chars() {
                if !c.is_alphanumeric() && c != '-' && c != '_' {
                    return Err(format!(
                        "Invalid character in Batch ID: '{}'. Allowed: alphanumeric, '-', '_'",
                        c
                    ));
                }
            }
            Ok(Self(id_str))
        }
    }

    impl AsRef<str> for BatchId {
        fn as_ref(&self) -> &str {
            &self.0
        }
    }
}

#[cfg(test)]
mod test {
    use crate::reader::concurrent_lines;
    use crate::test::{create_test_queue, localstack};

    use super::*;

    #[tokio::test]
    async fn test_local() {
        // create a localstack container and a queue
        let (endpoint, container) = localstack().await.unwrap();
        let queue_url = create_test_queue(&container, "batch-send-test", false)
            .await
            .unwrap();

        // create a reader for the batch messages
        let reader = tokio::io::BufReader::new(std::io::Cursor::new(
            b"first line\nsecond line\nthird\n".to_vec(),
        ));
        let (handle, rx) = concurrent_lines(reader, 10);

        // create a sqs batch client and send the batch messages to the queue
        let sqs = SqsBatch::local(&queue_url, Some(endpoint)).await;
        sqs.send(rx).await;

        // wait for the handle to complete to ensure the messages are sent
        handle.await.unwrap();

        // create a sqs client and receive the messages from the queue
        let client = aws_sdk_sqs::Client::new(&sqs.aws_config);
        let receive_output = client
            .receive_message()
            .queue_url(&queue_url)
            .max_number_of_messages(10)
            .send()
            .await
            .unwrap();

        // verify the messages were received without respect to the order
        let messages = receive_output.messages.unwrap_or_default();
        assert_eq!(messages.len(), 3);

        let received_bodies = messages
            .iter()
            .filter_map(|m| m.body())
            .collect::<Vec<&str>>();

        assert_eq!(received_bodies.len(), 3);
        assert!(received_bodies.contains(&"first line"));
        assert!(received_bodies.contains(&"second line"));
        assert!(received_bodies.contains(&"third"));

        container.stop().await.unwrap();

        ()
    }
}
