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
//! # async fn example() {
//! // Load AWS configuration
//! let config = aws_config::from_env().load().await;
//!
//! // Create a DLQ client
//! let dlq = DeadLetterQueue::from_config(config, None);
//!
//! // List all queues
//! let queues = dlq.list().await;
//! for url in queues {
//!     println!("Queue: {}", url);
//! }
//! # }
//! ```

mod send;
mod sqs;

#[cfg(test)]
mod test_utils;

pub use sqs::*;
