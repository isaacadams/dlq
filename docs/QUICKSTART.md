```
docker compose up -d
AWS_PROFILE=localstack aws --endpoint-url http://localhost:4566 sqs create-queue --queue-name demo
cargo run -- database seed send_batch --items 10000 -p
cargo run -- --local send-batch 1 "http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/demo"
```
