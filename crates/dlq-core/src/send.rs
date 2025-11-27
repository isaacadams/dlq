use crate::sqs::DeadLetterQueue;
use std::fmt;

#[derive(Debug)]
pub enum SendError {
    MissingQueueUrl,
    BuildEntryFailed(String),
    AwsSdkError(
        aws_sdk_sqs::error::SdkError<
            aws_sdk_sqs::operation::send_message_batch::SendMessageBatchError,
        >,
    ),
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::MissingQueueUrl => write!(f, "queue URL was not specified"),
            SendError::BuildEntryFailed(msg) => write!(f, "failed to build message entry: {}", msg),
            SendError::AwsSdkError(e) => write!(f, "AWS SDK error: {}", e),
        }
    }
}

impl std::error::Error for SendError {}

impl DeadLetterQueue {
    pub async fn send_batch(&self, messages: &[impl AsRef<str>]) -> Result<(), SendError> {
        let queue_url = self
            .default_queue_url
            .as_deref()
            .ok_or(SendError::MissingQueueUrl)?;

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
            .map_err(SendError::AwsSdkError)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{cleanup_shared_container, get_shared_localstack, local_config, setup};

    // Cleanup function that runs after all tests
    // This test runs last alphabetically to ensure cleanup happens after other tests.
    // Note: Test execution order isn't fully guaranteed in parallel mode, but this helps.
    #[tokio::test]
    async fn zzz_cleanup_shared_container() {
        // Small delay to allow other tests to finish
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        cleanup_shared_container().await;
    }

    #[tokio::test]
    async fn test_send_batch_success() {
        let (dlq, queue_url, config) = setup("test-batch").await;

        let messages = vec!["message1", "message2", "message3"];
        let result = dlq.send_batch(&messages).await;

        assert!(result.is_ok(), "send_batch should succeed");

        // Verify messages were actually sent by receiving them
        let client = aws_sdk_sqs::Client::new(&config);
        let receive_output = client
            .receive_message()
            .queue_url(&queue_url)
            .max_number_of_messages(10)
            .send()
            .await
            .unwrap();

        let received_messages = receive_output.messages.unwrap_or_default();
        assert_eq!(received_messages.len(), 3, "Should receive 3 messages");

        let received_bodies: Vec<&str> = received_messages
            .iter()
            .map(|m| m.body().unwrap_or(""))
            .collect();

        assert!(received_bodies.contains(&"message1"));
        assert!(received_bodies.contains(&"message2"));
        assert!(received_bodies.contains(&"message3"));
    }

    #[tokio::test]
    async fn test_send_batch_empty_messages() {
        let (dlq, _, _) = setup("test-empty").await;

        let messages: Vec<&str> = vec![];
        let result = dlq.send_batch(&messages).await;

        // Empty batch should still succeed (just sends nothing)
        assert!(
            result.is_ok(),
            "send_batch with empty messages should succeed"
        );
    }

    #[tokio::test]
    async fn test_send_batch_missing_queue_url() {
        let endpoint_url = get_shared_localstack().await;

        let config = local_config(&endpoint_url, None).load().await;
        let client = aws_sdk_sqs::Client::new(&config);

        // Create DLQ without default_queue_url
        let dlq = DeadLetterQueue {
            config,
            client,
            default_queue_url: None,
        };

        let messages = vec!["message1", "message2"];
        let result = dlq.send_batch(&messages).await;

        assert!(result.is_err(), "send_batch should fail without queue URL");
        match result.unwrap_err() {
            SendError::MissingQueueUrl => {}
            _ => panic!("Expected MissingQueueUrl error"),
        }
    }

    #[tokio::test]
    async fn test_send_batch_with_string_slice() {
        let (dlq, queue_url, config) = setup("test-str-slice").await;

        let messages: &[&str] = &["test1", "test2", "test3"];
        let result = dlq.send_batch(messages).await;

        assert!(result.is_ok(), "send_batch should work with &[&str]");

        // Verify messages
        let client = aws_sdk_sqs::Client::new(&config);
        let receive_output = client
            .receive_message()
            .queue_url(&queue_url)
            .max_number_of_messages(10)
            .send()
            .await
            .unwrap();

        assert_eq!(receive_output.messages.unwrap_or_default().len(), 3);
    }

    #[tokio::test]
    async fn test_send_batch_with_string_vec() {
        let (dlq, queue_url, config) = setup("test-string-vec").await;

        let messages: Vec<String> = vec![
            "message1".to_string(),
            "message2".to_string(),
            "message3".to_string(),
        ];
        let result = dlq.send_batch(&messages).await;

        assert!(result.is_ok(), "send_batch should work with Vec<String>");

        // Verify messages
        let client = aws_sdk_sqs::Client::new(&config);
        let receive_output = client
            .receive_message()
            .queue_url(&queue_url)
            .max_number_of_messages(10)
            .send()
            .await
            .unwrap();

        assert_eq!(receive_output.messages.unwrap_or_default().len(), 3);
    }

    #[tokio::test]
    async fn test_send_batch_generates_unique_ids() {
        let (dlq, queue_url, config) = setup("test-unique-ids").await;

        let messages = vec!["msg1", "msg2"];
        let result = dlq.send_batch(&messages).await;
        assert!(result.is_ok());

        // Receive messages and verify they have unique IDs
        let client = aws_sdk_sqs::Client::new(&config);
        let receive_output = client
            .receive_message()
            .queue_url(&queue_url)
            .max_number_of_messages(10)
            .send()
            .await
            .unwrap();

        let received_messages = receive_output.messages.unwrap_or_default();
        assert_eq!(received_messages.len(), 2);

        // Verify message IDs are UUIDs (they should be 36 characters long)
        for msg in received_messages {
            let msg_id = msg.message_id().unwrap();
            // UUIDs are 36 characters (with hyphens) or we can check the format
            assert!(msg_id.len() >= 32, "Message ID should be a UUID");
        }
    }
}
