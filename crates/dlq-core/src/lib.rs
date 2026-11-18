//! # dlq-core
//!
//! Core library for interacting with AWS SQS dead letter queues.
//!
//! This crate provides the fundamental types and operations for working with SQS,
//! including listing queues, polling messages, and sending message batches.
//!
//! ## Features
//!
//! - **Queue Operations**: List all SQS queues in an AWS account
//! - **Message Polling**: Receive and deserialize messages from queues
//! - **Batch Sending**: Send multiple messages in efficient batches
//!
//! ## Example
//!
//! ```no_run
//! use dlq::DeadLetterQueue;
//!
//! # async fn example() -> Result<(), dlq::Error> {
//! // Load AWS configuration
//! let config = aws_config::from_env().load().await;
//!
//! // Create a DLQ client
//! let dlq = DeadLetterQueue::from_config(config);
//!
//! // List all queues
//! for url in dlq.list().await? {
//!     println!("Queue: {}", url);
//! }
//! # Ok(())
//! # }
//! ```

mod error;
mod redrive;
mod send;
mod sqs;

#[cfg(feature = "test-utils")]
#[doc(hidden)]
pub mod test_utils;

pub use error::{BatchEntryFailure, Error, Result};
pub use redrive::RedriveSummary;
pub use sqs::*;
