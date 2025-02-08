use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::process::Command;
use testcontainers::GenericImage;
use tokio::io::AsyncReadExt;

async fn localstack(
) -> Result<testcontainers::ContainerAsync<GenericImage>, testcontainers::TestcontainersError> {
    use testcontainers::{
        core::{ContainerPort, Mount, WaitFor},
        runners::AsyncRunner,
        GenericImage, ImageExt,
    };

    GenericImage::new("localstack/localstack", "latest")
        // Wait condition - using healthcheck equivalent
        .with_wait_for(WaitFor::message_on_stdout("Ready."))
        // Port mappings
        .with_exposed_port(ContainerPort::Tcp(4566))
        //.with_exposed_port_range(ContainerPort::Tcp(4510)..=ContainerPort::Tcp(4559))
        // Environment variables
        .with_env_var("HOSTNAME_EXTERNAL", "localstack")
        .with_env_var("SERVICES", "sqs:4576,s3")
        .with_env_var("DEBUG", "1")
        // Volume mounts
        .with_mount(Mount::bind_mount(
            "/var/run/docker.sock",
            "/var/run/docker.sock",
        ))
        .start()
        .await
}

async fn create_test_queue(
    container: &testcontainers::ContainerAsync<GenericImage>,
    debug: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let create_queue_command = testcontainers::core::ExecCommand::new([
        "awslocal",
        "sqs",
        "create-queue",
        "--queue-name",
        "test-queue",
    ])
    .with_container_ready_conditions(vec![testcontainers::core::WaitFor::message_on_stdout(
        "Ready.",
    )]);

    if debug {
        let mut output = container.exec(create_queue_command).await?;
        let mut stdout = String::new();
        let mut stderr = String::new();
        output.stdout().read_to_string(&mut stdout).await.unwrap();
        output.stderr().read_to_string(&mut stderr).await.unwrap();
        println!(
            "Queue creation command output:\nstdout: {}\nstderr: {}",
            stdout, stderr
        );
    }

    Ok(())
}

fn setup_localstack_env(host_port: u16) {
    std::env::set_var(
        "AWS_ENDPOINT_URL",
        format!("http://localhost:{}", host_port),
    );
    std::env::set_var("AWS_ACCESS_KEY_ID", "test");
    std::env::set_var("AWS_SECRET_ACCESS_KEY", "test");
    std::env::set_var("AWS_DEFAULT_REGION", "us-east-1");
}

#[tokio::test]
async fn command_does_not_exist() {
    let mut cmd = Command::cargo_bin("dlq").unwrap();

    cmd.arg("something");
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("error: unrecognized subcommand"));

    ()
}

#[tokio::test]
async fn list_queues() {
    let container = localstack().await.unwrap();

    // Get the actual host port that Docker mapped to the container's port
    let host_port = container.get_host_port_ipv4(4566).await.unwrap();
    setup_localstack_env(host_port);
    create_test_queue(&container, false).await.unwrap();

    let mut cmd = Command::cargo_bin("dlq").unwrap();

    cmd.arg("list");
    cmd.assert().success().stdout(predicate::str::contains(
        "http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/test-queue",
    ));

    ()
}
