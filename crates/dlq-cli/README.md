# DLQ CLI

A Rust CLI tool for working with AWS SQS dead letter queues.

## Prerequisites

Ensure your AWS credentials are configured:
- Via AWS CLI: `aws configure`
- Via environment: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_REGION`
- Via AWS profiles: `AWS_PROFILE=myprofile`

## Global Flags

| Flag | Description |
|------|-------------|
| `--local` | Use LocalStack mode (test credentials + `localhost:4566`) |
| `--endpoint <URL>` | Override AWS endpoint URL |

## Commands

### List Queues

List all SQS queues in your AWS account.

```bash
dlq list

# With a specific profile/region
AWS_PROFILE=production AWS_REGION=us-east-1 dlq list
```

### Poll Messages

Receive and display messages from a queue as JSON.

```bash
dlq poll https://sqs.us-east-1.amazonaws.com/123456789/my-dlq
```

### Show AWS Info

Display current AWS configuration (endpoint, region, credentials).

```bash
dlq info
```

### Send Messages (Interactive)

Interactive mode for sending messages to a queue. Reads lines from stdin and sends them to SQS.

```bash
dlq send https://sqs.us-east-1.amazonaws.com/123456789/my-queue
```

### Batch Send

Send job items to an SQS queue in high-throughput batches. Uses a SQLite database to track job progress and supports retries.

```bash
dlq send-batch <JOB_ID> <QUEUE_URL> [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--batch-size <N>` | 10 | Items per SQS batch (max: 10) |
| `--stage-size <N>` | auto | Items to stage from SQLite per round |
| `--concurrency <N>` | 5 | Parallel SQS batch sends |
| `--retry-limit <N>` | 0 | Max retries per item (0 = disabled) |
| `-n, --limit <N>` | all | Maximum items to process |
| `--dry-run` | - | Preview without sending |

**Example:**

```bash
# Send all items from job 1 to a queue
dlq send-batch 1 https://sqs.us-east-1.amazonaws.com/123456789/my-queue

# Dry run with limit
dlq send-batch 1 https://sqs.us-east-1.amazonaws.com/123456789/my-queue --dry-run -n 100

# With LocalStack
dlq --local send-batch 1 http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/demo
```

### Job Management

Jobs are stored in a local SQLite database (`sqs.db`) and track batches of items to be sent to SQS.

#### List Jobs

```bash
dlq job list
```

#### Check Job Status

```bash
dlq job status <JOB_ID>
```

Shows counts for each status: `pending`, `processing`, `done`, `failed`.

#### Load Items from JSONL

Load items from a JSONL file (one JSON value per line) or stdin.

```bash
# From file
dlq job load --name my-job items.jsonl

# From stdin
cat items.jsonl | dlq job load --name my-job
```

#### Reset Stale Items

Reset items stuck in `processing` state back to `pending`.

```bash
dlq job reset <JOB_ID>
```

### Database Utilities

#### Seed Test Data

Create a job with synthetic test items (useful for development/testing).

```bash
dlq database seed my-test-job --items 10000 -p
```

The `-p` flag shows a progress bar.

#### Clean Database

Delete the SQLite database files.

```bash
dlq database clean
```

## LocalStack Development

For local development with LocalStack:

```bash
# Start LocalStack
docker compose up -d

# Create a queue
aws --endpoint-url=http://localhost:4566 sqs create-queue --queue-name demo

# Use the CLI with --local flag
dlq --local list
dlq --local poll http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/demo

# Seed and send test data
dlq database seed test_job --items 1000 -p
dlq --local send-batch 1 http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/demo
```

## Environment Variables

| Variable | Description |
|----------|-------------|
| `AWS_PROFILE` | AWS profile to use |
| `AWS_REGION` | AWS region |
| `AWS_ACCESS_KEY_ID` | AWS access key |
| `AWS_SECRET_ACCESS_KEY` | AWS secret key |
