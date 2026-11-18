//! Moving messages out of a dead letter queue.

use std::collections::{HashMap, HashSet};

use aws_sdk_sqs::types::{
    DeleteMessageBatchRequestEntry, Message, MessageSystemAttributeName,
    SendMessageBatchRequestEntry,
};

use crate::error::{Error, Result};
use crate::sqs::DeadLetterQueue;

/// Seconds a received message stays hidden while it is being moved. Messages that
/// fail to move become visible in the source queue again after this.
const VISIBILITY_TIMEOUT: i32 = 60;

/// Outcome of [`DeadLetterQueue::redrive`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RedriveSummary {
    /// Messages sent to the destination and deleted from the source.
    pub moved: usize,
    /// Messages the destination rejected. They stay in the source queue.
    pub failed: usize,
}

impl DeadLetterQueue {
    /// Moves messages from `source_url` (usually a DLQ) to `destination_url`.
    ///
    /// Each message is sent with its body and message attributes, then deleted from
    /// the source only once the destination has accepted it, so a failure never loses
    /// messages (at worst one is duplicated). For FIFO destinations, the original
    /// `MessageGroupId` and `MessageDeduplicationId` are kept when present.
    ///
    /// Stops when the source is empty, `limit` messages have moved, or only messages
    /// that already failed in this run are left.
    ///
    /// AWS also offers server-side redrive (`StartMessageMoveTask`) for queues with a
    /// redrive policy; this method works for any pair of queues.
    ///
    /// # Errors
    ///
    /// Returns an error if receiving, sending, or deleting fails as a whole. Messages
    /// the destination rejects individually are counted in
    /// [`RedriveSummary::failed`] instead.
    pub async fn redrive(
        &self,
        source_url: &str,
        destination_url: &str,
        limit: Option<usize>,
    ) -> Result<RedriveSummary> {
        let fifo = destination_url.ends_with(".fifo");
        let mut summary = RedriveSummary::default();
        let mut failed_ids: HashSet<String> = HashSet::new();

        loop {
            let remaining = limit.map_or(10, |l| l.saturating_sub(summary.moved).min(10));
            if remaining == 0 {
                break;
            }

            let messages = self
                .client
                .receive_message()
                .queue_url(source_url)
                .max_number_of_messages(remaining as i32)
                .visibility_timeout(VISIBILITY_TIMEOUT)
                .message_attribute_names("All")
                .message_system_attribute_names(MessageSystemAttributeName::MessageGroupId)
                .message_system_attribute_names(MessageSystemAttributeName::MessageDeduplicationId)
                .send()
                .await
                .map_err(|e| Error::Receive {
                    queue_url: source_url.to_string(),
                    source: Box::new(e),
                })?
                .messages
                .unwrap_or_default();

            let messages: Vec<Message> = messages
                .into_iter()
                .filter(|m| !failed_ids.contains(m.message_id().unwrap_or_default()))
                .collect();
            if messages.is_empty() {
                break;
            }

            let entries = messages
                .iter()
                .enumerate()
                .map(|(i, m)| send_entry(i, m, fifo))
                .collect::<Result<Vec<_>, _>>()?;

            let output = self
                .client
                .send_message_batch()
                .queue_url(destination_url)
                .set_entries(Some(entries))
                .send()
                .await
                .map_err(|e| Error::Send {
                    queue_url: destination_url.to_string(),
                    source: Box::new(e),
                })?;

            for failure in &output.failed {
                if let Some(m) = failure
                    .id
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| messages.get(i))
                {
                    failed_ids.insert(m.message_id().unwrap_or_default().to_string());
                }
            }
            summary.failed += output.failed.len();

            let deletes = output
                .successful
                .iter()
                .filter_map(|s| {
                    let index: usize = s.id.parse().ok()?;
                    let handle = messages.get(index)?.receipt_handle()?;
                    Some(
                        DeleteMessageBatchRequestEntry::builder()
                            .id(&s.id)
                            .receipt_handle(handle)
                            .build(),
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            if deletes.is_empty() {
                continue;
            }

            let deleted = self
                .client
                .delete_message_batch()
                .queue_url(source_url)
                .set_entries(Some(deletes))
                .send()
                .await
                .map_err(|e| Error::DeleteBatch {
                    queue_url: source_url.to_string(),
                    source: Box::new(e),
                })?;
            summary.moved += deleted.successful.len();
            if !deleted.failed.is_empty() {
                // Already delivered; they will be delivered again on the next run.
                summary.failed += deleted.failed.len();
            }
        }

        Ok(summary)
    }
}

fn send_entry(
    index: usize,
    message: &Message,
    fifo: bool,
) -> Result<SendMessageBatchRequestEntry, aws_sdk_sqs::error::BuildError> {
    let mut entry = SendMessageBatchRequestEntry::builder()
        .id(index.to_string())
        .message_body(message.body().unwrap_or_default())
        .set_message_attributes(message.message_attributes.clone());

    if fifo {
        let attributes: HashMap<_, _> = message.attributes.clone().unwrap_or_default();
        let message_id = message.message_id().unwrap_or_default();
        entry = entry
            .message_group_id(
                attributes
                    .get(&MessageSystemAttributeName::MessageGroupId)
                    .map_or("dlq-redrive", String::as_str),
            )
            .message_deduplication_id(
                attributes
                    .get(&MessageSystemAttributeName::MessageDeduplicationId)
                    .map_or(message_id, String::as_str),
            );
    }

    entry.build()
}

#[cfg(test)]
mod tests {
    use aws_sdk_sqs::types::{MessageAttributeValue, QueueAttributeName};

    use crate::test_utils::SqsTestEnv;

    #[tokio::test]
    async fn test_redrive_moves_all_messages() {
        let env = SqsTestEnv::start().await;
        let dlq_url = env.create_queue("redrive-src").await;
        let target_url = env.create_queue("redrive-dst").await;
        let dlq = env.dlq();

        let messages: Vec<String> = (0..25).map(|i| format!("m{i}")).collect();
        for batch in messages.chunks(10) {
            dlq.send_batch(&dlq_url, batch).await.unwrap();
        }

        let summary = dlq.redrive(&dlq_url, &target_url, None).await.unwrap();
        assert_eq!(summary.moved, 25);
        assert_eq!(summary.failed, 0);

        let mut moved = env.receive_bodies(&target_url, 25).await;
        moved.sort();
        let mut expected = messages.clone();
        expected.sort();
        assert_eq!(moved, expected);
        assert!(env.receive_bodies(&dlq_url, 1).await.is_empty());
    }

    #[tokio::test]
    async fn test_redrive_respects_limit_and_keeps_attributes() {
        let env = SqsTestEnv::start().await;
        let dlq_url = env.create_queue("redrive-limit-src").await;
        let target_url = env.create_queue("redrive-limit-dst").await;

        for i in 0..5 {
            env.client()
                .send_message()
                .queue_url(&dlq_url)
                .message_body(format!("m{i}"))
                .message_attributes(
                    "origin",
                    MessageAttributeValue::builder()
                        .data_type("String")
                        .string_value("orders")
                        .build()
                        .unwrap(),
                )
                .send()
                .await
                .unwrap();
        }

        let summary = env
            .dlq()
            .redrive(&dlq_url, &target_url, Some(3))
            .await
            .unwrap();
        assert_eq!(summary.moved, 3);

        let received = env
            .client()
            .receive_message()
            .queue_url(&target_url)
            .max_number_of_messages(10)
            .message_attribute_names("All")
            .send()
            .await
            .unwrap()
            .messages
            .unwrap_or_default();
        assert_eq!(received.len(), 3);
        for m in &received {
            let origin = m.message_attributes().and_then(|a| a.get("origin"));
            assert_eq!(origin.and_then(|v| v.string_value()), Some("orders"));
        }
    }

    #[tokio::test]
    async fn test_redrive_to_fifo_queue() {
        let env = SqsTestEnv::start().await;
        let dlq_url = env.create_queue("redrive-fifo-src").await;
        let target_url = env
            .client()
            .create_queue()
            .queue_name(format!(
                "{}.fifo",
                crate::test_utils::unique_queue_name("redrive-fifo-dst")
            ))
            .attributes(QueueAttributeName::FifoQueue, "true")
            .send()
            .await
            .unwrap()
            .queue_url
            .unwrap();

        env.dlq().send_batch(&dlq_url, &["a", "b"]).await.unwrap();

        let summary = env
            .dlq()
            .redrive(&dlq_url, &target_url, None)
            .await
            .unwrap();
        assert_eq!(summary.moved, 2);
        assert_eq!(env.receive_bodies(&target_url, 2).await.len(), 2);
    }

    #[tokio::test]
    async fn test_purge_empties_queue() {
        let env = SqsTestEnv::start().await;
        let queue_url = env.create_queue("purge").await;
        env.dlq()
            .send_batch(&queue_url, &["a", "b", "c"])
            .await
            .unwrap();

        env.dlq().purge(&queue_url).await.unwrap();
        assert!(env.receive_bodies(&queue_url, 1).await.is_empty());
    }
}
