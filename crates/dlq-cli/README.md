# DLQ Cli Tool

### 1. List your SQS Urls

```bash
dlq list
```

### 2. fetch all messages in your dlq

```bash
# poll the given queue url
dlq poll <url>

# use environment variable
DLQ_URL=<url> dlq poll
```
