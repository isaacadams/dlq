//! Moving messages out of a dead letter queue.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use aws_sdk_sqs::types::{
    BatchResultErrorEntry, ChangeMessageVisibilityBatchRequestEntry,
    DeleteMessageBatchRequestEntry, Message, MessageSystemAttributeName, QueueAttributeName,
    SendMessageBatchRequestEntry,
};

use crate::error::{BatchEntryFailure, Error, Result};
use crate::sqs::DeadLetterQueue;

/// Seconds a received message stays hidden while it is being moved.
const VISIBILITY_TIMEOUT: i32 = 60;

/// Seconds a message that could not be moved is kept hidden, so the rest of the run
/// never receives (and never resends) it. Released at the end of the run.
const HOLD_SECONDS: i32 = 15 * 60;

/// Long-poll wait per receive. Any non-zero wait makes SQS query all of its servers,
/// so an empty response reliably means no messages are visible (short polling samples
/// a subset and can come back empty while messages remain).
const WAIT_TIME_SECONDS: i32 = 1;

/// Payload budget per `SendMessageBatch` request. Kept at the historical 256 KiB batch
/// limit so requests stay valid regardless of the queue's configured maximum.
const MAX_BATCH_BYTES: usize = 256 * 1024;

/// Outcome of [`DeadLetterQueue::redrive`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RedriveSummary {
    /// Messages sent to the destination and deleted from the source.
    pub moved: usize,
    /// Messages the destination rejected. They remain in the source queue.
    pub rejected: Vec<RedriveFailure>,
    /// Messages delivered to the destination that could not be deleted from the
    /// source. They remain in the source too, so redriving again delivers them twice.
    pub not_deleted: Vec<RedriveFailure>,
    /// FIFO message groups (source is a FIFO queue) that stopped at a rejected message.
    /// Later messages in these groups were not sent, to keep their order.
    pub blocked_groups: Vec<String>,
}

impl RedriveSummary {
    /// Whether every message that was attempted moved successfully.
    pub fn is_complete(&self) -> bool {
        self.rejected.is_empty() && self.not_deleted.is_empty() && self.blocked_groups.is_empty()
    }
}

/// A message that could not be fully moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedriveFailure {
    /// SQS message ID in the source queue.
    pub message_id: String,
    /// The error SQS returned for this message.
    pub error: BatchEntryFailure,
}

impl DeadLetterQueue {
    /// Moves messages from `source_url` (usually a DLQ) to `destination_url`.
    ///
    /// Each message is sent with its body and message attributes, then deleted from
    /// the source only once the destination has accepted it, so a failure never loses
    /// messages (at worst one is duplicated). For FIFO destinations, the original
    /// `MessageGroupId` is kept and each run uses a fresh `MessageDeduplicationId`, so
    /// SQS never discards a redriven message as a duplicate of its original send.
    ///
    /// Messages that cannot be moved are kept hidden for the rest of the run and made
    /// visible again at the end, so they are attempted once per run and the run ends
    /// when a long-poll receive finds nothing else, or `limit` messages have moved.
    /// For FIFO sources, a rejected message also stops the rest of its group (see
    /// [`RedriveSummary::blocked_groups`]). Messages are sent up to 10 at a time, so
    /// later messages of that group already in the same batch may still be delivered.
    ///
    /// AWS also offers server-side redrive (`StartMessageMoveTask`) for queues with a
    /// redrive policy; this method works for any pair of queues.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RedriveToSameQueue`] if both URLs identify the same queue
    /// (that would loop forever), and an error if receiving, sending, or deleting fails
    /// as a whole. Messages that fail individually are reported in the summary instead.
    pub async fn redrive(
        &self,
        source_url: &str,
        destination_url: &str,
        limit: Option<usize>,
    ) -> Result<RedriveSummary> {
        if self.is_same_queue(source_url, destination_url).await? {
            return Err(Error::RedriveToSameQueue {
                queue_url: source_url.to_string(),
            });
        }

        let mut run = Redrive {
            dlq: self,
            source_url,
            destination_url,
            source_fifo: source_url.ends_with(".fifo"),
            destination_fifo: destination_url.ends_with(".fifo"),
            dedup_suffix: run_nonce(),
            summary: RedriveSummary::default(),
            held: HashMap::new(),
        };
        let result = run.run(limit).await;

        // Always make held messages visible again, even if the run failed part way.
        let released = run.release_held().await;
        result?;
        released?;
        Ok(run.summary)
    }

    /// Whether two queue URLs identify the same queue. Decided from the URLs when
    /// possible; equivalent URLs with different hosts are compared by queue ARN.
    async fn is_same_queue(&self, a: &str, b: &str) -> Result<bool> {
        match same_queue_by_url(a, b) {
            Some(same) => Ok(same),
            None => {
                let (arn_a, arn_b) = tokio::try_join!(self.queue_arn(a), self.queue_arn(b))?;
                Ok(arn_a == arn_b)
            }
        }
    }

    async fn queue_arn(&self, queue_url: &str) -> Result<String> {
        self.client
            .get_queue_attributes()
            .queue_url(queue_url)
            .attribute_names(QueueAttributeName::QueueArn)
            .send()
            .await
            .map_err(|e| Error::QueueAttributes {
                queue_url: queue_url.to_string(),
                source: Box::new(e),
            })?
            .attributes
            .and_then(|mut a| a.remove(&QueueAttributeName::QueueArn))
            .ok_or_else(|| Error::MissingQueueArn {
                queue_url: queue_url.to_string(),
            })
    }
}

/// State for one redrive run.
struct Redrive<'a> {
    dlq: &'a DeadLetterQueue,
    source_url: &'a str,
    destination_url: &'a str,
    source_fifo: bool,
    destination_fifo: bool,
    dedup_suffix: String,
    summary: RedriveSummary,
    /// Messages kept hidden for the rest of the run: message ID -> latest receipt handle.
    held: HashMap<String, String>,
}

impl Redrive<'_> {
    async fn run(&mut self, limit: Option<usize>) -> Result<()> {
        loop {
            let remaining = limit.map_or(10, |l| l.saturating_sub(self.summary.moved).min(10));
            if remaining == 0 {
                return Ok(());
            }

            let messages = self.receive(remaining).await?;
            if messages.is_empty() {
                return Ok(());
            }

            // A held message can come back if the run outlasts HOLD_SECONDS; hide it
            // again instead of resending it.
            let (again, fresh): (Vec<_>, Vec<_>) = messages
                .into_iter()
                .partition(|m| self.held.contains_key(message_id(m)));
            self.hold(&again).await?;

            for chunk in chunk_by_size(&fresh, MAX_BATCH_BYTES) {
                self.move_chunk(&fresh[chunk]).await?;
            }
        }
    }

    async fn receive(&self, max: usize) -> Result<Vec<Message>> {
        Ok(self
            .dlq
            .client
            .receive_message()
            .queue_url(self.source_url)
            .max_number_of_messages(max as i32)
            .visibility_timeout(VISIBILITY_TIMEOUT)
            .wait_time_seconds(WAIT_TIME_SECONDS)
            .message_attribute_names("All")
            .message_system_attribute_names(MessageSystemAttributeName::MessageGroupId)
            .send()
            .await
            .map_err(|e| Error::Receive {
                queue_url: self.source_url.to_string(),
                source: Box::new(e),
            })?
            .messages
            .unwrap_or_default())
    }

    /// Sends one size-bounded chunk, then deletes what the destination accepted.
    async fn move_chunk(&mut self, chunk: &[Message]) -> Result<()> {
        // FIFO source: messages after a rejected one in the same group are not sent,
        // so the group's order is kept. They are held and released at the end.
        let (blocked, to_send): (Vec<&Message>, Vec<&Message>) = chunk
            .iter()
            .partition(|m| self.source_fifo && self.is_blocked(m));
        self.hold(blocked).await?;
        if to_send.is_empty() {
            return Ok(());
        }

        let entries = to_send
            .iter()
            .enumerate()
            .map(|(i, m)| send_entry(i, m, self.destination_fifo, &self.dedup_suffix))
            .collect::<Result<Vec<_>, _>>()?;
        let output = self
            .dlq
            .client
            .send_message_batch()
            .queue_url(self.destination_url)
            .set_entries(Some(entries))
            .send()
            .await
            .map_err(|e| Error::Send {
                queue_url: self.destination_url.to_string(),
                source: Box::new(e),
            })?;

        let successful: HashSet<&str> = output.successful.iter().map(|s| s.id.as_str()).collect();
        let (accepted, rejected) = classify(&to_send, &successful, output.failed.clone());
        for (message, failure) in &rejected {
            if self.source_fifo
                && let Some(group) = group_id(message)
                && !self.summary.blocked_groups.iter().any(|g| g == group)
            {
                self.summary.blocked_groups.push(group.to_string());
            }
            self.summary.rejected.push(RedriveFailure {
                message_id: message_id(message).to_string(),
                error: failure.clone(),
            });
        }
        self.hold(rejected.iter().map(|(m, _)| *m)).await?;

        self.delete(&accepted).await
    }

    async fn delete(&mut self, messages: &[&Message]) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let entries = messages
            .iter()
            .enumerate()
            .map(|(i, m)| {
                DeleteMessageBatchRequestEntry::builder()
                    .id(i.to_string())
                    .receipt_handle(m.receipt_handle().unwrap_or_default())
                    .build()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let output = self
            .dlq
            .client
            .delete_message_batch()
            .queue_url(self.source_url)
            .set_entries(Some(entries))
            .send()
            .await
            .map_err(|e| Error::DeleteBatch {
                queue_url: self.source_url.to_string(),
                source: Box::new(e),
            })?;

        let deleted: HashSet<&str> = output.successful.iter().map(|s| s.id.as_str()).collect();
        self.summary.moved += deleted.len();

        // Delivered but still in the source: hold them so this run never sends them
        // again, and report them so the caller knows a re-run would duplicate them.
        let (_, not_deleted) = classify(messages, &deleted, output.failed.clone());
        for (message, failure) in &not_deleted {
            self.summary.not_deleted.push(RedriveFailure {
                message_id: message_id(message).to_string(),
                error: failure.clone(),
            });
        }
        self.hold(not_deleted.iter().map(|(m, _)| *m)).await
    }

    fn is_blocked(&self, message: &Message) -> bool {
        group_id(message).is_some_and(|g| self.summary.blocked_groups.iter().any(|b| b == g))
    }

    /// Keeps messages hidden for HOLD_SECONDS and remembers them for release.
    async fn hold<'m>(&mut self, messages: impl IntoIterator<Item = &'m Message>) -> Result<()> {
        let messages: Vec<&Message> = messages.into_iter().collect();
        for m in &messages {
            self.held.insert(
                message_id(m).to_string(),
                m.receipt_handle().unwrap_or_default().to_string(),
            );
        }
        let handles: Vec<&str> = messages
            .iter()
            .map(|m| m.receipt_handle().unwrap_or_default())
            .collect();
        self.change_visibility(&handles, HOLD_SECONDS).await
    }

    /// Makes every held message visible again.
    async fn release_held(&mut self) -> Result<()> {
        let handles: Vec<String> = self.held.drain().map(|(_, h)| h).collect();
        let handles: Vec<&str> = handles.iter().map(String::as_str).collect();
        self.change_visibility(&handles, 0).await
    }

    async fn change_visibility(&self, handles: &[&str], seconds: i32) -> Result<()> {
        for batch in handles.chunks(10) {
            let entries = batch
                .iter()
                .enumerate()
                .map(|(i, handle)| {
                    ChangeMessageVisibilityBatchRequestEntry::builder()
                        .id(i.to_string())
                        .receipt_handle(*handle)
                        .visibility_timeout(seconds)
                        .build()
                })
                .collect::<Result<Vec<_>, _>>()?;
            self.dlq
                .client
                .change_message_visibility_batch()
                .queue_url(self.source_url)
                .set_entries(Some(entries))
                .send()
                .await
                .map_err(|e| Error::ChangeVisibility {
                    queue_url: self.source_url.to_string(),
                    source: Box::new(e),
                })?;
        }
        Ok(())
    }
}

/// Splits a batch response into accepted messages and rejected ones with their error.
/// Entry IDs are indexes into `messages`. A message SQS reported as neither accepted
/// nor failed is treated as rejected, so it is never deleted.
fn classify<'m>(
    messages: &[&'m Message],
    successful: &HashSet<&str>,
    failed: Vec<BatchResultErrorEntry>,
) -> (Vec<&'m Message>, Vec<(&'m Message, BatchEntryFailure)>) {
    let mut failures: HashMap<String, BatchEntryFailure> = failed
        .into_iter()
        .map(|f| (f.id.clone(), BatchEntryFailure::from(f)))
        .collect();

    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    for (i, message) in messages.iter().enumerate() {
        let id = i.to_string();
        if successful.contains(id.as_str()) {
            accepted.push(*message);
        } else {
            let failure = failures.remove(&id).unwrap_or_else(|| BatchEntryFailure {
                index: i,
                code: "MissingFromResponse".to_string(),
                message: Some("SQS did not report a result for this entry".to_string()),
                sender_fault: false,
            });
            rejected.push((*message, failure));
        }
    }
    (accepted, rejected)
}

/// Splits messages into consecutive ranges whose estimated payload stays within
/// `max_bytes` (and at most 10 entries). A single oversized message gets its own range.
fn chunk_by_size(messages: &[Message], max_bytes: usize) -> Vec<Range<usize>> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut bytes = 0;
    for (i, message) in messages.iter().enumerate() {
        let size = payload_size(message);
        if i > start && (bytes + size > max_bytes || i - start == 10) {
            chunks.push(start..i);
            start = i;
            bytes = 0;
        }
        bytes += size;
    }
    if start < messages.len() {
        chunks.push(start..messages.len());
    }
    chunks
}

/// SQS counts the body plus each message attribute's name, type, and value.
fn payload_size(message: &Message) -> usize {
    let attributes: usize = message
        .message_attributes()
        .map(|attrs| {
            attrs
                .iter()
                .map(|(name, value)| {
                    name.len()
                        + value.data_type().len()
                        + value.string_value().map_or(0, str::len)
                        + value.binary_value().map_or(0, |b| b.as_ref().len())
                })
                .sum()
        })
        .unwrap_or(0);
    message.body().map_or(0, str::len) + attributes
}

/// Decides from the URLs alone when possible: `Some(true)` for the same URL,
/// `Some(false)` when the account/queue-name paths differ, `None` when only the
/// hosts differ (e.g. `sqs.us-east-1.amazonaws.com` vs `queue.amazonaws.com`).
fn same_queue_by_url(a: &str, b: &str) -> Option<bool> {
    fn split(url: &str) -> (String, &str) {
        let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        (host.to_ascii_lowercase(), path.trim_end_matches('/'))
    }
    let (host_a, path_a) = split(a);
    let (host_b, path_b) = split(b);
    if path_a != path_b {
        Some(false)
    } else if host_a == host_b {
        Some(true)
    } else {
        None
    }
}

/// A suffix unique to this run, so FIFO deduplication IDs never match an earlier send.
fn run_nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:x}")
}

fn message_id(message: &Message) -> &str {
    message.message_id().unwrap_or_default()
}

fn group_id(message: &Message) -> Option<&str> {
    message
        .attributes()?
        .get(&MessageSystemAttributeName::MessageGroupId)
        .map(String::as_str)
}

fn send_entry(
    index: usize,
    message: &Message,
    fifo: bool,
    dedup_suffix: &str,
) -> Result<SendMessageBatchRequestEntry, aws_sdk_sqs::error::BuildError> {
    let mut entry = SendMessageBatchRequestEntry::builder()
        .id(index.to_string())
        .message_body(message.body().unwrap_or_default())
        .set_message_attributes(message.message_attributes.clone());

    if fifo {
        // A new deduplication ID per run: reusing the original one would make SQS
        // accept-and-drop the message if it was first sent less than 5 minutes ago.
        entry = entry
            .message_group_id(group_id(message).unwrap_or("dlq-redrive"))
            .message_deduplication_id(format!("{}-{dedup_suffix}", message_id(message)));
    }

    entry.build()
}

#[cfg(test)]
mod unit_tests {
    use std::collections::HashSet;

    use aws_sdk_sqs::types::{BatchResultErrorEntry, Message, MessageSystemAttributeName};

    use super::*;

    fn message(id: &str, body_len: usize) -> Message {
        Message::builder()
            .message_id(id)
            .receipt_handle(format!("handle-{id}"))
            .body("x".repeat(body_len))
            .build()
    }

    #[test]
    fn chunks_respect_size_and_count() {
        let sizes = [100, 100, 300, 50, 50, 400, 600];
        let messages: Vec<_> = sizes
            .iter()
            .enumerate()
            .map(|(i, len)| message(&i.to_string(), *len))
            .collect();
        // 500-byte budget; the 600-byte message exceeds it on its own.
        assert_eq!(chunk_by_size(&messages, 500), vec![0..3, 3..6, 6..7]);

        let many: Vec<_> = (0..25).map(|i| message(&i.to_string(), 1)).collect();
        assert_eq!(
            chunk_by_size(&many, usize::MAX),
            vec![0..10, 10..20, 20..25]
        );
        assert!(chunk_by_size(&[], 500).is_empty());
    }

    #[test]
    fn same_queue_decided_from_urls_when_possible() {
        let q = "https://sqs.us-east-1.amazonaws.com/123456789012/orders";
        assert_eq!(same_queue_by_url(q, q), Some(true));
        assert_eq!(
            same_queue_by_url(
                q,
                "HTTPS://SQS.us-east-1.amazonaws.com/123456789012/orders/"
            ),
            Some(true)
        );
        assert_eq!(
            same_queue_by_url(
                q,
                "https://sqs.us-east-1.amazonaws.com/123456789012/orders-dlq"
            ),
            Some(false)
        );
        // Same path, different host: needs an ARN lookup.
        assert_eq!(
            same_queue_by_url(q, "https://queue.amazonaws.com/123456789012/orders"),
            None
        );
    }

    #[test]
    fn classify_splits_accepted_and_rejected() {
        let messages = [message("a", 1), message("b", 1), message("c", 1)];
        let refs: Vec<&Message> = messages.iter().collect();
        let successful: HashSet<&str> = ["0"].into();
        let failed = vec![
            BatchResultErrorEntry::builder()
                .id("1")
                .code("InvalidParameterValue")
                .sender_fault(true)
                .build()
                .unwrap(),
        ];

        let (accepted, rejected) = classify(&refs, &successful, failed);
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].message_id(), Some("a"));
        // "c" is missing from the response entirely: never treat it as accepted.
        let codes: Vec<_> = rejected
            .iter()
            .map(|(m, f)| (m.message_id().unwrap(), f.code.as_str()))
            .collect();
        assert_eq!(
            codes,
            [("b", "InvalidParameterValue"), ("c", "MissingFromResponse")]
        );
    }

    #[test]
    fn fifo_entries_get_fresh_dedup_ids_and_keep_group() {
        let original = Message::builder()
            .message_id("m1")
            .body("hi")
            .attributes(MessageSystemAttributeName::MessageGroupId, "g1")
            .attributes(MessageSystemAttributeName::MessageDeduplicationId, "orig")
            .build();

        let first = send_entry(0, &original, true, "run1").unwrap();
        let second = send_entry(0, &original, true, "run2").unwrap();
        assert_eq!(first.message_group_id(), Some("g1"));
        assert_ne!(first.message_deduplication_id(), Some("orig"));
        assert_ne!(
            first.message_deduplication_id(),
            second.message_deduplication_id()
        );

        let standard = send_entry(0, &original, false, "run1").unwrap();
        assert_eq!(standard.message_group_id(), None);
        assert_eq!(standard.message_deduplication_id(), None);
    }
}

#[cfg(test)]
mod tests {
    use aws_sdk_sqs::types::{MessageAttributeValue, QueueAttributeName};

    use crate::test_utils::SqsTestEnv;

    async fn create_fifo_queue(env: &SqsTestEnv, prefix: &str) -> String {
        env.client()
            .create_queue()
            .queue_name(format!(
                "{}.fifo",
                crate::test_utils::unique_queue_name(prefix)
            ))
            .attributes(QueueAttributeName::FifoQueue, "true")
            .send()
            .await
            .unwrap()
            .queue_url
            .unwrap()
    }

    async fn send_fifo(env: &SqsTestEnv, queue_url: &str, body: &str, group: &str, dedup: &str) {
        env.client()
            .send_message()
            .queue_url(queue_url)
            .message_body(body)
            .message_group_id(group)
            .message_deduplication_id(dedup)
            .send()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_fifo_redrive_is_not_dropped_as_duplicate() {
        // A message first sent with dedup ID "d1" moves to the DLQ and is redriven
        // within SQS's 5-minute dedup window. Reusing "d1" would be accepted-and-dropped.
        let env = SqsTestEnv::start().await;
        let source = create_fifo_queue(&env, "dedup-src").await;
        let destination = create_fifo_queue(&env, "dedup-dst").await;
        send_fifo(&env, &destination, "original", "g", "d1").await;
        env.receive_bodies(&destination, 1).await; // consumed and failed processing...
        send_fifo(&env, &source, "original", "g", "d1").await; // ...and ended up in the DLQ

        let summary = env
            .dlq()
            .redrive(&source, &destination, None)
            .await
            .unwrap();
        assert_eq!(summary.moved, 1);
        // Visible again after the first receive's visibility timeout would hide it, so
        // look for the redriven copy among everything currently in the queue.
        let attributes = env
            .client()
            .get_queue_attributes()
            .queue_url(&destination)
            .attribute_names(QueueAttributeName::ApproximateNumberOfMessages)
            .attribute_names(QueueAttributeName::ApproximateNumberOfMessagesNotVisible)
            .send()
            .await
            .unwrap()
            .attributes
            .unwrap_or_default();
        let total: usize = attributes
            .values()
            .map(|v| v.parse::<usize>().unwrap())
            .sum();
        assert_eq!(total, 2, "redriven message was dropped as a duplicate");
    }

    #[tokio::test]
    async fn test_redrive_splits_large_messages_across_batches() {
        let env = SqsTestEnv::start().await;
        let source = env.create_queue("large-src").await;
        let destination = env.create_queue("large-dst").await;

        // 10 x 100 KiB is well over one batch's 256 KiB budget.
        let messages: Vec<String> = (0..10)
            .map(|i| format!("{i}{}", "x".repeat(100 * 1024)))
            .collect();
        for message in &messages {
            env.dlq().send_batch(&source, &[message]).await.unwrap();
        }

        let summary = env
            .dlq()
            .redrive(&source, &destination, None)
            .await
            .unwrap();
        assert_eq!(summary.moved, 10);
        assert!(summary.is_complete(), "{summary:?}");
        assert_eq!(env.receive_bodies(&destination, 10).await.len(), 10);
    }

    #[tokio::test]
    async fn test_redrive_fifo_source_moves_every_group_in_order() {
        let env = SqsTestEnv::start().await;
        let source = create_fifo_queue(&env, "groups-src").await;
        let destination = create_fifo_queue(&env, "groups-dst").await;
        for i in 0..6 {
            for group in ["a", "b"] {
                send_fifo(
                    &env,
                    &source,
                    &format!("{group}{i}"),
                    group,
                    &format!("{group}{i}"),
                )
                .await;
            }
        }

        let summary = env
            .dlq()
            .redrive(&source, &destination, None)
            .await
            .unwrap();
        assert_eq!(summary.moved, 12);
        assert!(summary.is_complete(), "{summary:?}");

        // Delete as we read: FIFO queues hold back a group while earlier messages
        // from it are still in flight.
        let mut received = Vec::new();
        while received.len() < 12 {
            let messages = env
                .client()
                .receive_message()
                .queue_url(&destination)
                .max_number_of_messages(10)
                .wait_time_seconds(1)
                .send()
                .await
                .unwrap()
                .messages
                .unwrap_or_default();
            assert!(!messages.is_empty(), "only got {received:?}");
            for m in messages {
                env.dlq()
                    .delete(&destination, m.receipt_handle().unwrap())
                    .await
                    .unwrap();
                received.push(m.body.unwrap());
            }
        }
        for group in ["a", "b"] {
            let order: Vec<_> = received.iter().filter(|b| b.starts_with(group)).collect();
            let expected: Vec<_> = (0..6).map(|i| format!("{group}{i}")).collect();
            assert_eq!(order, expected.iter().collect::<Vec<_>>());
        }
    }

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
        assert!(summary.is_complete(), "{summary:?}");

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
    async fn test_redrive_to_same_queue_is_rejected() {
        let env = SqsTestEnv::start().await;
        let queue_url = env.create_queue("redrive-self").await;
        env.dlq().send_batch(&queue_url, &["a"]).await.unwrap();

        // A different but equivalent URL for the same queue is still caught.
        let alias = queue_url.replace("127.0.0.1", "localhost");
        for destination in [&queue_url, &alias] {
            let result = env.dlq().redrive(&queue_url, destination, None).await;
            assert!(
                matches!(result, Err(crate::Error::RedriveToSameQueue { .. })),
                "{destination}: {result:?}"
            );
        }
        assert_eq!(env.receive_bodies(&queue_url, 1).await, ["a"]);
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
