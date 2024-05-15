use anyhow::Context;
use aws_config::SdkConfig;
use aws_sdk_sqs as sqs;
use sqs::types::DeleteMessageBatchRequestEntry;

pub async fn list() {
    let dlq = DeadLetterQueue::new().await;
    let queues = dlq.list().await;
    println!("{}", queues.join(","));
}

pub async fn poll(url: Option<&str>) {
    let dlq = DeadLetterQueue::new().await;
    dlq.poll(url).await;
}

pub async fn receive(
    client: &aws_sdk_sqs::Client,
    queue_url: &str,
) -> anyhow::Result<aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput> {
    let result = client
        .receive_message()
        .set_queue_url(Some(queue_url.to_string()))
        .set_max_number_of_messages(Some(10))
        .set_visibility_timeout(Some(15))
        .message_system_attribute_names(sqs::types::MessageSystemAttributeName::All)
        //.set_wait_time_seconds(Some(3))
        .send()
        .await;

    result.context("failed to receive messages")
}

#[derive(Clone)]
struct DeadLetterQueue {
    pub _config: SdkConfig,
    pub client: sqs::Client,
    pub default_queue_url: Option<String>,
}

impl DeadLetterQueue {
    pub async fn new() -> Self {
        let config = aws_config::load_from_env().await;
        let client = aws_sdk_sqs::Client::new(&config);

        Self {
            _config: config,
            client,
            default_queue_url: std::env::var("DLQ_URL").ok(),
        }
    }

    pub async fn _clear(&self, url: String, message_id: String, receipt_handle: String) {
        self.client
            .delete_message_batch()
            .set_queue_url(Some(url))
            .set_entries(Some(vec![DeleteMessageBatchRequestEntry::builder()
                .set_id(Some(message_id))
                .set_receipt_handle(Some(receipt_handle))
                .build()
                .unwrap()]))
            .send()
            .await
            .unwrap();
    }

    pub async fn list(&self) -> Vec<String> {
        let mut queues = Vec::new();

        let mut output = self.client.list_queues().send().await.unwrap();
        loop {
            if let Some(mut list) = output.queue_urls {
                queues.append(&mut list);
            }

            let Some(token) = output.next_token else {
                break;
            };

            output = self
                .client
                .list_queues()
                .set_next_token(Some(token))
                .send()
                .await
                .unwrap();
        }

        queues
    }

    pub async fn poll(&self, queue_url: Option<&str>) {
        let url = queue_url
            .or(self.default_queue_url.as_deref())
            .expect("failed: queue url was not specified");
        loop {
            let output = receive(&self.client, url).await.unwrap();

            // if none, that suggests the whole queue has been received recently
            let Some(messages) = output.messages else {
                return;
            };

            for m in messages {
                println!(
                    "{}",
                    serde_json::to_string(&MessageModel::from_aws_message(m)).unwrap()
                );
            }
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct MessageModel {
    pub message_id: String,
    receipt_handle: String,
    md5_of_body: String,
    pub body: String,
    md5_of_message_attributes: Option<String>,
    attributes: Option<String>,
    message_attributes: Option<String>,
}

impl MessageModel {
    /// https://docs.aws.amazon.com/AWSSimpleQueueService/latest/APIReference/API_Message.html
    pub fn from_aws_message(message: aws_sdk_sqs::types::Message) -> Self {
        Self {
            message_id: message.message_id.expect("missing message_id"),
            receipt_handle: message.receipt_handle.expect("missing receipt_handle"),
            md5_of_body: message.md5_of_body.expect("missing md5_of_body"),
            body: message.body.expect("missing body"),
            md5_of_message_attributes: message.md5_of_message_attributes,
            attributes: None,         //value.attributes,
            message_attributes: None, //value.message_attributes,
        }
    }
}
