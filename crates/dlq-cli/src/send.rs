pub fn stdin(
    buffer_size: usize,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::mpsc::Receiver<String>,
) {
    use tokio::io::{self, AsyncBufReadExt, BufReader};
    let stdin = io::stdin();
    let reader = BufReader::new(stdin);
    let (tx, rx) = tokio::sync::mpsc::channel::<String>(buffer_size);

    let task = tokio::spawn(async move {
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            match tx.send(line).await {
                Ok(_) => (),
                Err(e) => {
                    eprintln!("error sending message to channel: {}", e);
                    break;
                }
            }
        }
    });
    (task, rx)
}

pub async fn run() {
    let (h_stdin, rx_stdin) = stdin(100);
    sqs_batch_send(
        "http://localhost:4566",
        // aws --endpoint-url http://localhost:4566 sqs create-queue --queue-name demo --output json
        "http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/demo",
        rx_stdin,
    )
    .await;
    h_stdin.await.unwrap();
}

pub async fn sqs_batch_send(
    aws_endpoint: &str,
    aws_sqs_queue_url: &'static str,
    rx: tokio::sync::mpsc::Receiver<String>,
) {
    let client = std::sync::Arc::new(sqs(aws_endpoint).await);
    use pumps::{Concurrency, Pipeline};
    let (mut rx_pipeline, h_pipeline) = Pipeline::from(rx)
        .filter_map(
            |x| async move {
                if x.len() < 1 {
                    return None;
                }

                println!("{}", x);

                let entry = aws_sdk_sqs::types::SendMessageBatchRequestEntry::builder()
                    .id(BatchId::new(uuid::Uuid::new_v4()).unwrap().0)
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
                async move {
                    job(&*client, aws_sqs_queue_url, entries).await;
                }
            },
            Concurrency::concurrent_ordered(10),
        )
        .build();

    while let Some(_) = rx_pipeline.recv().await {}
    h_pipeline.await.unwrap();
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

async fn sqs(endpoint: &str) -> aws_sdk_sqs::Client {
    aws_sdk_sqs::Client::new(
        &aws_config::from_env()
            .region(
                // supports loading region from known env variables
                aws_config::meta::region::RegionProviderChain::default_provider()
                    .or_else(aws_config::Region::from_static("us-east-1")),
            )
            .credentials_provider(aws_sdk_sqs::config::Credentials::new(
                "test", "test", None, None, "static",
            ))
            .endpoint_url(endpoint)
            .load()
            .await,
    )
}

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
