# DLQ - Dead Letter Queue CLI

A powerful Rust CLI tool for working with AWS SQS dead letter queues. Supports listing queues, polling messages, batch sending with job management, and local development with LocalStack.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## Features

- 📋 **List SQS Queues** - View all queues in your AWS account
- 📨 **Poll Messages** - Receive and display messages from any queue
- 🚀 **Batch Sending** - High-throughput batch message sending with progress tracking
- 💾 **Job Management** - SQLite-backed job queue for reliable message processing
- 🔄 **Retry Support** - Automatic retries with configurable limits
- 🧪 **LocalStack Support** - First-class local development support

## Installation

```bash
# Install from crates.io
cargo install dlq
```

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

# Or set the queue URL via environment variable
DLQ_URL=https://sqs.us-east-1.amazonaws.com/123456789/my-queue dlq poll

# Override AWS profile/region
AWS_PROFILE=production AWS_REGION=eu-west-1 dlq list
```

### Local Development with LocalStack

```bash
# Start LocalStack (requires Docker)
docker compose up -d

# Create a test queue
aws --endpoint-url=http://localhost:4566 sqs create-queue --queue-name demo

# Use dlq with --local flag
dlq --local list
dlq --local poll http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/demo
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
| `dlq info` | Display AWS configuration info |
| `dlq send` | Interactive message sending |
| `dlq send-batch` | Batch send messages from a job |
| `dlq job` | Job management commands |
| `dlq database` | Database utilities |

For detailed command documentation, see the [CLI README](crates/dlq-cli/README.md).

## License

[MIT](LICENSE)

## Repository

https://github.com/isaacadams/dlq
