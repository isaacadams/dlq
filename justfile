default:
    just -l

dev:
    mkdir -p .localstack
    docker compose up -d

set positional-arguments
@awslocal *args='':
    aws --endpoint-url=http://localhost:4566 $@

create_s3_bucket:
    aws --endpoint-url=http://localhost:4572 s3 mb s3://demo-bucket

create_queue:
    just awslocal sqs create-queue
