default:
    just -l

set positional-arguments
@awslocal *args='':
    @aws --endpoint-url=http://localhost:4566 $@

dev:
    mkdir -p .localstack
    docker compose up -d
    just awslocal sqs create-queue --queue-name test
    #just generate_messages 100 | \
    #just load_test_events http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/test

create_queue:
    just awslocal sqs create-queue --queue-name test

# just load_test_events http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/test
load_test_events url:
    #!/bin/bash
    rust-parallel -r '(.*)' \
        --jobs 25 \
        --progress-bar \
        --shell \
        'aws --endpoint-url=http://localhost:4566 sqs send-message-batch --queue-url "{{url}}" --entries '\'{1}\'''

# just generate_messages 1000 > messages.json
generate_messages amount:
    #!/bin/bash
    for ((i=1;i<={{amount}};i++)); do
    echo -n '['
    echo -n '{"Id":"'$(uuidgen)'","MessageBody":"Test Event '$i'_a"},'
    echo -n '{"Id":"'$(uuidgen)'","MessageBody":"Test Event '$i'_b"},'
    echo -n '{"Id":"'$(uuidgen)'","MessageBody":"Test Event '$i'_c"},'
    echo -n '{"Id":"'$(uuidgen)'","MessageBody":"Test Event '$i'_d"},'
    echo -n '{"Id":"'$(uuidgen)'","MessageBody":"Test Event '$i'_e"},'
    echo -n '{"Id":"'$(uuidgen)'","MessageBody":"Test Event '$i'_f"},'
    echo -n '{"Id":"'$(uuidgen)'","MessageBody":"Test Event '$i'_g"},'
    echo -n '{"Id":"'$(uuidgen)'","MessageBody":"Test Event '$i'_h"},'
    echo -n '{"Id":"'$(uuidgen)'","MessageBody":"Test Event '$i'_i"},'
    echo -n '{"Id":"'$(uuidgen)'","MessageBody":"Test Event '$i'_j"}'
    echo ']'
    done
