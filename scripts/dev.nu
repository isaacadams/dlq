$env.AWS_ENDPOINT_URL = "http://localhost:4566"

# > nu scripts/dev.nu
def main [] {
    docker compose down
    mkdir .localstack
    docker compose up -d
    aws sqs create-queue --queue-name test
    main send messages 100 test
}

# Generate a batch of test messages in SQS batch format
# > nu scripts/dev.nu generate messages 3
def "main generate messages" [batches: int = 3] {
    seq 1 $batches | each { |i|
        [
            (message $"($i)_a")
            (message $"($i)_b")
            (message $"($i)_c")
            (message $"($i)_d")
            (message $"($i)_e")
        ]
    } | flatten
}

def message [id] {
    {Id: $id, MessageBody: $"Test Event ($id)"} | to json -r
}

# Send messages to the SQS queue in LocalStack
# Example usage:
# > nu scripts/dev.nu send messages 100 test
def "main send messages" [
    batch: int = 3,                      # Number of batch messages to send
    queue_name: string = "test"          # Name of the queue
] {
    let queue_url = (aws sqs get-queue-url --queue-name $queue_name | from json).QueueUrl

    main generate messages $batch
        | chunks 10 # can only send 10 at a time to sqs
        | par-each {|batch|
            let entries = ($batch | each { $in | from json }) | to json -r
            aws sqs send-message-batch --queue-url $queue_url --entries $entries
        }
}
