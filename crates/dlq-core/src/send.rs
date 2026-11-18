//! Batch message sending functionality for SQS.

use crate::error::{BatchEntryFailure, Error, Result};
use crate::sqs::DeadLetterQueue;
use aws_sdk_sqs::types::{BatchResultErrorEntry, SendMessageBatchRequestEntry};

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
    ///
    /// # Errors
    ///
    /// - [`Error::Send`] - The SQS API call failed
    /// - [`Error::PartialSend`] - SQS rejected some entries (the rest were sent)
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
    pub async fn send_batch(&self, queue_url: &str, messages: &[impl AsRef<str>]) -> Result<()> {
        // Early return for empty batches - SQS doesn't allow empty batch requests
        if messages.is_empty() {
            return Ok(());
        }

        // Entry ids are batch indexes, so failures map straight back to the input.
        let entries = messages
            .iter()
            .enumerate()
            .map(|(i, message)| {
                SendMessageBatchRequestEntry::builder()
                    .id(i.to_string())
                    .message_body(message.as_ref())
                    .build()
            })
            .collect::<Result<Vec<_>, _>>()?;

        let output = self
            .client
            .send_message_batch()
            .queue_url(queue_url)
            .set_entries(Some(entries))
            .send()
            .await
            .map_err(|e| Error::Send {
                queue_url: queue_url.to_string(),
                source: Box::new(e),
            })?;

        check_batch_failures(queue_url, messages.len(), output.failed)
    }
}

/// Converts a batch response's `failed` entries into [`Error::PartialSend`].
pub(crate) fn check_batch_failures(
    queue_url: &str,
    total: usize,
    failed: Vec<BatchResultErrorEntry>,
) -> Result<()> {
    if failed.is_empty() {
        return Ok(());
    }
    Err(Error::PartialSend {
        queue_url: queue_url.to_string(),
        total,
        failed: failed
            .into_iter()
            .map(|f| BatchEntryFailure {
                index: f.id.parse().unwrap_or(usize::MAX),
                code: f.code,
                message: f.message,
                sender_fault: f.sender_fault,
            })
            .collect(),
    })
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
    async fn test_send_batch_reports_rejected_entries() {
        let env = SqsTestEnv::start().await;
        let queue_url = env.create_queue("test-partial").await;

        // SQS rejects bodies containing characters outside its allowed set
        // (InvalidMessageContents); the valid message is still sent.
        let result = env.dlq().send_batch(&queue_url, &["ok", "bad\u{0}"]).await;

        match result {
            Err(crate::Error::PartialSend { total, failed, .. }) => {
                assert_eq!(total, 2);
                assert_eq!(failed.len(), 1);
                assert_eq!(failed[0].index, 1);
            }
            other => panic!("expected PartialSend, got {other:?}"),
        }
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
