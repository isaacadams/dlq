# Local Development Quickstart

This guide walks through setting up a local development environment using
[ElasticMQ](https://github.com/softwaremill/elasticmq), a lightweight SQS-compatible server.

## Prerequisites

- Docker and Docker Compose
- AWS CLI (for creating queues)
- Rust toolchain (for building from source)

## Setup

### 1. Start the Local SQS Server

```bash
docker compose up -d
```

This starts ElasticMQ on `localhost:4566`, the endpoint `dlq --local` uses by default.

### 2. Create a Test Queue

```bash
aws --endpoint-url=http://localhost:4566 sqs create-queue --queue-name demo
```

### 3. Verify the Queue

```bash
# List queues using the CLI (release builds keep target/ smaller on disk)
cargo run --release -- --local list
```

This workspace prefers **release** builds. Prefer `cargo build --release` /
`cargo run --release`, or the aliases `cargo b` / `cargo r`.

## Example Workflows

### Batch Sending Test Data

```bash
# Seed the database with test items
cargo run --release -- database seed send_batch --items 10000 -p

# Send items to the queue
cargo run --release -- --local send-batch 1 "http://localhost:4566/000000000000/demo"

# Check job status
cargo run --release -- job status 1
```

### Polling Messages

```bash
# Poll messages from the queue
cargo run --release -- --local poll "http://localhost:4566/000000000000/demo"
```

### Loading Custom Data

```bash
# Create a JSONL file with your data
echo '{"id": 1, "message": "hello"}' > items.jsonl
echo '{"id": 2, "message": "world"}' >> items.jsonl

# Load into a job
cargo run --release -- job load --name my-job items.jsonl

# Send to queue
cargo run --release -- --local send-batch 1 "http://localhost:4566/000000000000/demo"
```

## Cleanup

```bash
# Remove database files
cargo run --release -- database clean

# Stop the local SQS server
docker compose down
```

## Troubleshooting

### Queue URL Format

Local queue URLs follow this format:
```
http://localhost:4566/000000000000/<queue-name>
```

### Connection Issues

If you can't connect to the local SQS server:
1. Ensure Docker is running: `docker ps`
2. Check the server logs: `docker compose logs sqs`
3. Verify the port is accessible: `aws --endpoint-url=http://localhost:4566 sqs list-queues`
