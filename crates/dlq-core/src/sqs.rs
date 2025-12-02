//! SQS client wrapper and message types for dead letter queue operations.

use anyhow::Context;
use aws_config::SdkConfig;
use aws_sdk_sqs as sqs;
use sqs::types::DeleteMessageBatchRequestEntry;

/// Receives messages from an SQS queue.
///
/// Retrieves up to 10 messages at a time with a 15-second visibility timeout.
/// Messages become invisible to other consumers during this period.
///
/// # Arguments
///
/// * `client` - The SQS client to use for the request
/// * `queue_url` - The URL of the queue to receive messages from
///
/// # Returns
///
/// The raw SQS receive message output containing the messages and metadata.
///
/// # Errors
///
/// Returns an error if the SQS API call fails.
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

/// Client for interacting with AWS SQS dead letter queues.
///
/// Provides high-level operations for listing queues, polling messages,
/// and clearing messages from queues.
///
/// # Example
///
/// ```no_run
/// use dlq::DeadLetterQueue;
///
/// # async fn example() {
/// let config = aws_config::from_env().load().await;
/// let dlq = DeadLetterQueue::from_config(config);
///
/// // List all queues
/// let queues = dlq.list().await;
///
/// // Poll messages from a queue
/// dlq.poll("https://sqs.us-east-1.amazonaws.com/123456789/my-dlq").await;
/// # }
/// ```
#[derive(Clone)]
pub struct DeadLetterQueue {
    /// The AWS SDK configuration used for SQS operations
    pub config: SdkConfig,
    /// The SQS client instance
    pub client: sqs::Client,
}

impl DeadLetterQueue {
    /// Creates a DeadLetterQueue from a pre-built AWS SDK config.
    ///
    /// This is the preferred constructor as it allows the caller to configure
    /// credentials and endpoints based on their needs (e.g., `--local` flag for LocalStack).
    ///
    /// # Arguments
    ///
    /// * `config` - Pre-configured AWS SDK configuration
    ///
    /// # Example
    ///
    /// ```no_run
    /// use dlq::DeadLetterQueue;
    ///
    /// # async fn example() {
    /// let config = aws_config::from_env().load().await;
    /// let dlq = DeadLetterQueue::from_config(config);
    ///
    /// // List all queues
    /// let queues = dlq.list().await;
    /// # }
    /// ```
    pub fn from_config(config: SdkConfig) -> Self {
        let client = sqs::Client::new(&config);
        Self { config, client }
    }

    /// Deletes a message from the queue using batch delete.
    ///
    /// This is primarily used internally to acknowledge processed messages.
    ///
    /// # Arguments
    ///
    /// * `url` - The queue URL
    /// * `message_id` - The SQS message ID
    /// * `receipt_handle` - The receipt handle from when the message was received
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

    /// Lists all SQS queue URLs in the AWS account.
    ///
    /// Handles pagination automatically, returning all queues regardless of count.
    ///
    /// # Returns
    ///
    /// A vector of queue URL strings.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use dlq::DeadLetterQueue;
    ///
    /// # async fn example() {
    /// let config = aws_config::from_env().load().await;
    /// let dlq = DeadLetterQueue::from_config(config);
    ///
    /// for queue_url in dlq.list().await {
    ///     println!("Found queue: {}", queue_url);
    /// }
    /// # }
    /// ```
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

    /// Polls messages from a queue and prints them as JSON.
    ///
    /// Continuously receives messages until the queue is empty or the maximum
    /// number of attempts (10) is reached. Each message is printed to stdout
    /// as a JSON object.
    ///
    /// # Arguments
    ///
    /// * `queue_url` - The URL of the queue to poll messages from
    ///
    /// # Example
    ///
    /// ```no_run
    /// use dlq::DeadLetterQueue;
    ///
    /// # async fn example() {
    /// let config = aws_config::from_env().load().await;
    /// let dlq = DeadLetterQueue::from_config(config);
    ///
    /// // Poll a specific queue
    /// dlq.poll("https://sqs.us-east-1.amazonaws.com/123/my-dlq").await;
    /// # }
    /// ```
    pub async fn poll(&self, queue_url: &str) {
        let url = queue_url;
        let max_tries = 10;
        let mut tries = 0;
        loop {
            tries += 1;
            if tries > max_tries {
                return;
            }

            let output = receive(&self.client, url).await.unwrap();

            // if none, that suggests the whole queue has been received recently
            let Some(messages) = output.messages else {
                return;
            };

            if messages.is_empty() {
                return;
            }

            for m in messages {
                println!(
                    "{}",
                    serde_json::to_string(&MessageModel::from_aws_message(m)).unwrap()
                );
            }
        }
    }
}

/// Serializable representation of an SQS message.
///
/// Contains the essential fields from an SQS message, formatted for
/// JSON output when polling queues.
#[derive(Clone, Debug, serde::Serialize)]
pub struct MessageModel {
    /// Unique identifier for the message assigned by SQS
    pub message_id: String,
    /// Handle used to delete or change visibility of the message
    receipt_handle: String,
    /// MD5 digest of the message body for integrity verification
    md5_of_body: String,
    /// The actual message content
    pub body: String,
    /// MD5 digest of message attributes (if any)
    md5_of_message_attributes: Option<String>,
    /// System attributes (currently not populated)
    attributes: Option<String>,
    /// Custom message attributes (currently not populated)
    message_attributes: Option<String>,
}

impl MessageModel {
    /// Converts an AWS SDK Message into a MessageModel.
    ///
    /// Extracts the required fields from the SQS message structure.
    ///
    /// # Panics
    ///
    /// Panics if the message is missing required fields (message_id, receipt_handle,
    /// md5_of_body, or body).
    ///
    /// # See Also
    ///
    /// - [AWS SQS Message API Reference](https://docs.aws.amazon.com/AWSSimpleQueueService/latest/APIReference/API_Message.html)
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
