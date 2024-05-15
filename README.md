# DLQ Cli Tool

Make sure your aws credentials are setup properly.

### 1. List your SQS Urls

```bash
dlq list
# change or set aws profile/region on the fly
AWS_PROFILE=<profile> AWS_REGION=us-east-1 dlq list
```

### 2. fetch all messages in your dlq

```bash
# poll the given queue url
dlq poll <url>
# use environment variable
DLQ_URL=<url> dlq poll
```
