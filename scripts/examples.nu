def main [] {}

# download all messages in a dead letter queue
# > nu scripts/examples.nu poll
def "main poll" [
    profile: string = "localstack",
    queue: string = "http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/test",
] {
    $env.AWS_PROFILE = $profile
    if $profile == "localstack" {
        $env.AWS_ENDPOINT_URL = "http://localhost:4566"
    }
    open queue.db | query db $"drop table if exists `($profile)`;"
    cargo run poll $queue
        | lines 
        | each { $in | from json } 
        | into sqlite queue.db -t $profile
}

# get queue url via queue name
# > nu scripts/examples.nu get-queue-url <name>
def "main get-queue-url" [name: string] {
    aws sqs get-queue-url --queue-name $name
}
