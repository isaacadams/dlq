default:
    just -l

dev:
    mkdir -p .localstack
    docker compose up -d

set positional-arguments
@awslocal *args='':
    @aws --endpoint-url=http://localhost:4566 $@

create_queue:
    just awslocal sqs create-queue --queue-name test

# just load_test_events http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/test 10
load_test_events url amount:
    #!/bin/bash
    for ((i=1;i<={{amount}};i++)); do
    aws --endpoint-url=http://localhost:4566 sqs send-message \
        --queue-url "{{url}}" \
        --message-body "Test Event $i"
    done
