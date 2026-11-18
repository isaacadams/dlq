//! Batch message sending functionality for SQS.

use crate::sqs::DeadLetterQueue;
use std::fmt;

/// Errors that can occur when sending messages to SQS.
#[derive(Debug)]
pub enum SendError {
    /// Failed to build a valid SQS message entry.
    BuildEntryFailed(String),
    /// An error occurred in the AWS SDK.
    AwsSdkError(
        Box<
            aws_sdk_sqs::error::SdkError<
                aws_sdk_sqs::operation::send_message_batch::SendMessageBatchError,
            >,
        >,
    ),
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::BuildEntryFailed(msg) => write!(f, "failed to build message entry: {}", msg),
            SendError::AwsSdkError(e) => write!(f, "AWS SDK error: {}", e),
        }
    }
}

impl std::error::Error for SendError {}

impl DeadLetterQueue {
    /// Sends multiple messages to SQS in a single batch request.
    ///
    /// Uses the SQS `SendMessageBatch` API to efficiently send up to 10 messages
    /// at once. Each message is assigned a unique UUID as its batch entry ID.
    ///
    /// # Arguments
    ///
    /// * `queue_url` - The URL of the SQS queue to send messages to
    /// * `messages` - A slice of message bodies to send. Each item must implement
    ///   `AsRef<str>`.
    ///
    /// # Returns
    ///
    /// * `Ok(())` - All messages were sent successfully
    /// * `Err(SendError)` - An error occurred during the send operation
    ///
    /// # Errors
    ///
    /// - `SendError::BuildEntryFailed` - A message entry couldn't be constructed
    /// - `SendError::AwsSdkError` - The SQS API returned an error
    ///
    /// # Example
    ///
    /// ```no_run
    /// use dlq::DeadLetterQueue;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let config = aws_config::from_env().load().await;
    /// let dlq = DeadLetterQueue::from_config(config);
    ///
    /// // Send multiple messages
    /// dlq.send_batch("https://sqs.us-east-1.amazonaws.com/123/my-queue", &["message 1", "message 2", "message 3"]).await?;
    ///
    /// // Empty batch is handled gracefully
    /// let empty: Vec<String> = vec![];
    /// dlq.send_batch("https://sqs.us-east-1.amazonaws.com/123/my-queue", &empty).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_batch(
        &self,
        queue_url: &str,
        messages: &[impl AsRef<str>],
    ) -> Result<(), SendError> {
        // Early return for empty batches - SQS doesn't allow empty batch requests
        if messages.is_empty() {
            return Ok(());
        }

        let entries: Result<Vec<_>, SendError> = messages
            .iter()
            .map(|message| {
                let id = uuid::Uuid::new_v4().to_string();
                let body = message.as_ref().to_string();
                aws_sdk_sqs::types::SendMessageBatchRequestEntry::builder()
                    .id(id)
                    .message_body(body)
                    .build()
                    .map_err(|e| SendError::BuildEntryFailed(e.to_string()))
            })
            .collect();

        let entries = entries?;

        self.client
            .send_message_batch()
            .queue_url(queue_url)
            .set_entries(Some(entries))
            .send()
            .await
            .map_err(|e| SendError::AwsSdkError(Box::new(e)))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::test_utils::SqsTestEnv;

    #[tokio::test]
    async fn test_send_batch_success() {
        let env = SqsTestEnv::start().await;
        let queue_url = env.create_queue("test-batch").await;

        let messages = vec!["message1", "message2", "message3"];
        let result = env.dlq().send_batch(&queue_url, &messages).await;
        assert!(result.is_ok(), "send_batch should succeed");

        // Verify messages were actually sent by receiving them
        let mut received = env.receive_bodies(&queue_url, 3).await;
        received.sort();
        assert_eq!(received, ["message1", "message2", "message3"]);
    }

    #[tokio::test]
    async fn test_send_batch_empty() {
        let env = SqsTestEnv::start().await;
        let queue_url = env.create_queue("test-empty").await;

        let messages: Vec<String> = vec![];
        let result = env.dlq().send_batch(&queue_url, &messages).await;

        assert!(result.is_ok(), "send_batch should handle empty batch");
    }
}
