//! Error types for SQS operations.

use aws_sdk_sqs::error::SdkError;
use aws_sdk_sqs::operation::{
    delete_message::DeleteMessageError, delete_message_batch::DeleteMessageBatchError,
    get_queue_attributes::GetQueueAttributesError, list_queues::ListQueuesError,
    purge_queue::PurgeQueueError, receive_message::ReceiveMessageError,
    send_message_batch::SendMessageBatchError,
};

/// Result alias for dlq-core operations.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors returned by [`DeadLetterQueue`](crate::DeadLetterQueue) operations.
///
/// SQS API failures keep the underlying AWS SDK error as their
/// [`source`](std::error::Error::source), so callers can inspect service error codes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// `ListQueues` failed.
    #[error("failed to list queues")]
    ListQueues(#[source] Box<SdkError<ListQueuesError>>),

    /// `ReceiveMessage` failed.
    #[error("failed to receive messages from {queue_url}")]
    Receive {
        queue_url: String,
        #[source]
        source: Box<SdkError<ReceiveMessageError>>,
    },

    /// `SendMessageBatch` failed as a whole.
    #[error("failed to send messages to {queue_url}")]
    Send {
        queue_url: String,
        #[source]
        source: Box<SdkError<SendMessageBatchError>>,
    },

    /// `SendMessageBatch` succeeded, but SQS rejected some of the entries.
    #[error("SQS rejected {} of {total} messages sent to {queue_url}: {}", failed.len(), summarize(failed))]
    PartialSend {
        queue_url: String,
        total: usize,
        failed: Vec<BatchEntryFailure>,
    },

    /// `DeleteMessage` failed.
    #[error("failed to delete message from {queue_url}")]
    Delete {
        queue_url: String,
        #[source]
        source: Box<SdkError<DeleteMessageError>>,
    },

    /// `DeleteMessageBatch` failed.
    #[error("failed to delete messages from {queue_url}")]
    DeleteBatch {
        queue_url: String,
        #[source]
        source: Box<SdkError<DeleteMessageBatchError>>,
    },

    /// `PurgeQueue` failed.
    #[error("failed to purge {queue_url}")]
    Purge {
        queue_url: String,
        #[source]
        source: Box<SdkError<PurgeQueueError>>,
    },

    /// `GetQueueAttributes` failed.
    #[error("failed to read attributes of {queue_url}")]
    QueueAttributes {
        queue_url: String,
        #[source]
        source: Box<SdkError<GetQueueAttributesError>>,
    },

    /// SQS did not return a `QueueArn` for the queue.
    #[error("SQS returned no QueueArn for {queue_url}")]
    MissingQueueArn { queue_url: String },

    /// Redrive source and destination are the same queue, which would loop forever.
    #[error("cannot redrive {queue_url} into itself; source and destination are the same queue")]
    RedriveToSameQueue { queue_url: String },

    /// A request entry could not be built (e.g. a required field was missing).
    #[error("failed to build request: {0}")]
    Build(#[from] aws_sdk_sqs::error::BuildError),
}

/// One entry SQS rejected in a batch request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchEntryFailure {
    /// Index of the entry in the batch that was sent.
    pub index: usize,
    /// SQS error code, e.g. `InvalidParameterValue`.
    pub code: String,
    /// Human-readable reason, if SQS gave one.
    pub message: Option<String>,
    /// Whether the failure was the caller's fault (retrying won't help).
    pub sender_fault: bool,
}

fn summarize(failed: &[BatchEntryFailure]) -> String {
    let mut codes: Vec<&str> = failed.iter().map(|f| f.code.as_str()).collect();
    codes.sort_unstable();
    codes.dedup();
    codes.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_send_message_lists_distinct_codes() {
        let failure = |index, code: &str| BatchEntryFailure {
            index,
            code: code.to_string(),
            message: None,
            sender_fault: true,
        };
        let error = Error::PartialSend {
            queue_url: "http://localhost/000000000000/q".to_string(),
            total: 5,
            failed: vec![
                failure(1, "InvalidMessageContents"),
                failure(3, "InvalidMessageContents"),
                failure(4, "InternalError"),
            ],
        };
        assert_eq!(
            error.to_string(),
            "SQS rejected 3 of 5 messages sent to http://localhost/000000000000/q: \
             InternalError, InvalidMessageContents"
        );
    }
}
