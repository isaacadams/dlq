# Local Development Quickstart

This guide walks through setting up a local development environment using LocalStack.

## Prerequisites

- Docker and Docker Compose
- AWS CLI (for creating queues)
- Rust toolchain (for building from source)

## Setup

### 1. Start LocalStack

```bash
docker compose up -d
```

This starts LocalStack with SQS enabled on `localhost:4566`.

### 2. Create a Test Queue

```bash
# Using the localstack profile (if configured) or with explicit endpoint
aws --endpoint-url=http://localhost:4566 sqs create-queue --queue-name demo
```

### 3. Verify the Queue

```bash
# List queues using the CLI
cargo run -- --local list
```

## Example Workflows

### Batch Sending Test Data

```bash
# Seed the database with test items
cargo run -- database seed send_batch --items 10000 -p

# Send items to the queue
cargo run -- --local send-batch 1 "http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/demo"

# Check job status
cargo run -- job status 1
```

### Polling Messages

```bash
# Poll messages from the queue
cargo run -- --local poll "http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/demo"
```

### Loading Custom Data

```bash
# Create a JSONL file with your data
echo '{"id": 1, "message": "hello"}' > items.jsonl
echo '{"id": 2, "message": "world"}' >> items.jsonl

# Load into a job
cargo run -- job load --name my-job items.jsonl

# Send to queue
cargo run -- --local send-batch 1 "http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/demo"
```

## Cleanup

```bash
# Remove database files
cargo run -- database clean

# Stop LocalStack
docker compose down
```

## Troubleshooting

### Queue URL Format

LocalStack queue URLs follow this format:
```
http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/<queue-name>
```

### Connection Issues

If you can't connect to LocalStack:
1. Ensure Docker is running: `docker ps`
2. Check LocalStack logs: `docker compose logs localstack`
3. Verify the port is accessible: `curl http://localhost:4566/_localstack/health`
