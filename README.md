# DLQ - Dead Letter Queue CLI

A powerful Rust CLI tool for working with AWS SQS dead letter queues. Supports listing queues, polling messages, batch sending with job management, and local development against a local SQS emulator.

[![CI](https://github.com/isaacadams/dlq/actions/workflows/ci.yml/badge.svg)](https://github.com/isaacadams/dlq/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/dlq.svg)](https://crates.io/crates/dlq)
[![docs.rs](https://img.shields.io/docsrs/dlq-core?label=docs.rs%20dlq-core)](https://docs.rs/dlq-core)
[![MSRV](https://img.shields.io/badge/MSRV-1.94.1-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## Features

- 📋 **List SQS Queues** - View all queues in your AWS account
- 📨 **Poll Messages** - Receive and display messages from any queue
- ♻️ **Redrive & Purge** - Move messages out of a DLQ back to their source queue, or clear a queue
- 🚀 **Batch Sending** - High-throughput batch message sending with progress tracking
- 💾 **Job Management** - SQLite-backed job queue for reliable message processing
- 🔄 **Retry Support** - Automatic retries with configurable limits
- 🧪 **Local Development** - `--local` mode for SQS emulators (ElasticMQ, LocalStack)

## Installation

```bash
# Install from crates.io
cargo install dlq
```

Prebuilt binaries for Linux, macOS, and Windows are attached to each
[`dlq` GitHub release](https://github.com/isaacadams/dlq/releases).

## Quick Start

### Prerequisites

Ensure your AWS credentials are configured:

```bash
# Option 1: AWS CLI configuration
aws configure

# Option 2: Environment variables
export AWS_ACCESS_KEY_ID=your_key
export AWS_SECRET_ACCESS_KEY=your_secret
export AWS_REGION=us-east-1
```

### Basic Usage

```bash
# List all SQS queues
dlq list

# Poll messages from a queue
dlq poll https://sqs.us-east-1.amazonaws.com/123456789/my-queue

# Move messages from a DLQ back to its source queue
dlq redrive https://sqs.us-east-1.amazonaws.com/123456789/my-dlq \
            https://sqs.us-east-1.amazonaws.com/123456789/my-queue

# Override AWS profile/region
AWS_PROFILE=production AWS_REGION=eu-west-1 dlq list
```

### Local Development

```bash
# Start a local SQS server (ElasticMQ on localhost:4566, requires Docker)
docker compose up -d

# Create a test queue
aws --endpoint-url=http://localhost:4566 sqs create-queue --queue-name demo

# Use dlq with --local flag
dlq --local list
dlq --local poll http://localhost:4566/000000000000/demo
```

For more detailed local development instructions, see [docs/QUICKSTART.md](docs/QUICKSTART.md).

## Project Structure

This is a Cargo workspace with two crates:

| Crate | Description |
|-------|-------------|
| [`dlq-cli`](crates/dlq-cli/README.md) | The CLI application with full command documentation |
| `dlq-core` | Core library for SQS operations |

## CLI Commands Overview

| Command | Description |
|---------|-------------|
| `dlq list` | List all SQS queue URLs |
| `dlq poll <url>` | Poll and display messages from a queue |
| `dlq redrive <source> <destination>` | Move messages between queues (e.g. DLQ → source) |
| `dlq purge <url>` | Delete every message in a queue (asks for confirmation) |
| `dlq info` | Display AWS configuration info |
| `dlq send` | Interactive message sending |
| `dlq send-batch` | Batch send messages from a job |
| `dlq job` | Job management commands |
| `dlq database` | Database utilities |
| `dlq completions <shell>` | Print a shell completion script |
| `dlq man` | Print the man page |

For detailed command documentation, see the [CLI README](crates/dlq-cli/README.md).

## License

[MIT](LICENSE)

## Repository

https://github.com/isaacadams/dlq
