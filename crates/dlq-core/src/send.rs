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
    use crate::test_utils::{create_test_queue, local_config, localstack, unique_queue_name};

    #[tokio::test]
    async fn test_send_batch_success() {
        let (endpoint_url, container) = localstack().await.unwrap();
        let queue_name = unique_queue_name("test-batch");
        create_test_queue(&container, &queue_name, false)
            .await
            .unwrap();

        let config = local_config(&endpoint_url, None).load().await;
        let dlq = DeadLetterQueue {
            config: local_config(&endpoint_url, None).load().await,
            client: aws_sdk_sqs::Client::new(&config),
            default_queue_url: Some(format!(
                "http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/{queue_name}"
            )),
        };

        let messages = vec!["message1", "message2", "message3"];
        let result = dlq.send_batch(&messages).await;

        assert!(result.is_ok(), "send_batch should succeed");

        // Verify messages were actually sent by receiving them
        let client = aws_sdk_sqs::Client::new(&config);
        let receive_output = client
            .receive_message()
            .queue_url(format!(
                "http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/{queue_name}"
            ))
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

        container.stop().await.unwrap();
    }
}
