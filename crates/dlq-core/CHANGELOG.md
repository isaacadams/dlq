# Changelog

All notable changes to `dlq-core` are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.0.0]

### Breaking changes
- Operations return `dlq::Result<T>` with a typed `dlq::Error` instead of panicking
  (`list`, `poll`, `receive`) or returning an unexported `SendError` (`send_batch`).
  SQS failures keep the AWS SDK error as their `source()`.
- `poll()` returns the received messages (`Vec<MessageModel>`) instead of printing them.
- `_clear()` is replaced by `delete(queue_url, receipt_handle)`.
- Minimum supported Rust version is 1.94.1 (required by the AWS SDK); edition 2024.
- The AWS SDK's legacy hyper 0.14 / rustls 0.21 HTTP client is no longer enabled.

### Added
- `redrive(source, destination, limit)` moves messages between queues, deleting each
  from the source only after the destination accepts it. Keeps message attributes
  and FIFO group/deduplication IDs.
- `purge(queue_url)`.
- `Error::PartialSend`: `send_batch` now reports entries SQS rejected (previously
  ignored silently).
- `MessageModel::receipt_handle` is public.
- Credentials from `aws login` are supported (`credentials-login`).

### Fixed
- `MessageModel::from_aws_message` no longer panics on missing fields.
- Crate-level documentation example now compiles.

## [1.0.1]

Initial published release.
