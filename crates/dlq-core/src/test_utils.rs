use aws_sdk_sqs::config::Credentials;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use testcontainers::ContainerAsync;
use testcontainers_modules::{
    localstack::LocalStack,
    testcontainers::{runners::AsyncRunner, Image, ImageExt, TestcontainersError},
};
use tokio::io::AsyncReadExt;
use tokio::sync::{Mutex, OnceCell};

// Register cleanup to run when the process exits
static CLEANUP_REGISTERED: OnceLock<()> = OnceLock::new();

pub fn local_config(endpoint_url: &str, region: Option<&'static str>) -> aws_config::ConfigLoader {
    aws_config::defaults(aws_config::BehaviorVersion::latest())
        .endpoint_url(endpoint_url)
        .region(region.unwrap_or("us-east-1"))
        .credentials_provider(Credentials::new("test", "test", None, None, "static"))
}

pub async fn localstack() -> Result<(String, ContainerAsync<LocalStack>), TestcontainersError> {
    let request = LocalStack::default()
        .with_tag("latest")
        .with_env_var("SERVICES", "sqs:4576,s3")
        .with_env_var("SKIP_SSL_CERT_DOWNLOAD", "1");
    let container = request.start().await?;

    let host_ip = container.get_host().await?;
    let host_port = container.get_host_port_ipv4(4566).await?;
    let endpoint_url = format!("http://{host_ip}:{host_port}");

    Ok((endpoint_url, container))
}

pub async fn create_test_queue<I: Image>(
    container: &ContainerAsync<I>,
    name: &str,
    debug: bool,
) -> Result<(), TestcontainersError> {
    let create_queue_command = testcontainers::core::ExecCommand::new([
        "awslocal",
        "sqs",
        "create-queue",
        "--queue-name",
        name,
    ])
    .with_container_ready_conditions(vec![testcontainers::core::WaitFor::message_on_stdout(
        "AWS sqs.CreateQueue => 200",
    )]);

    let mut output = container.exec(create_queue_command).await?;

    if debug {
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

// Shared LocalStack container across all tests for better performance
// Container is initialized once and reused across all tests
static SHARED_CONTAINER: OnceCell<Mutex<(String, ContainerAsync<LocalStack>)>> =
    OnceCell::const_new();
static CLEANUP_CALLED: AtomicBool = AtomicBool::new(false);

/// Get the shared LocalStack endpoint URL, initializing the container if needed.
/// This reuses a single container across all tests for better performance.
pub async fn get_shared_localstack() -> String {
    // Register cleanup on first initialization
    CLEANUP_REGISTERED.get_or_init(|| {
        // Use a simple approach: containers will be cleaned up by testcontainers
        // when the process exits. For explicit cleanup, call cleanup_shared_container()
        // after all tests complete (e.g., in a test harness or CI script).
    });

    let container_data = SHARED_CONTAINER
        .get_or_init(|| async {
            let (endpoint_url, container) = localstack().await.unwrap();
            Mutex::new((endpoint_url, container))
        })
        .await;

    let guard = container_data.lock().await;
    guard.0.clone()
}

/// Get the shared LocalStack container, ensuring it's initialized.
pub async fn get_shared_container() -> &'static Mutex<(String, ContainerAsync<LocalStack>)> {
    // Ensure initialization
    let _ = get_shared_localstack().await;
    SHARED_CONTAINER.get().unwrap()
}

/// Generate a unique queue name for testing, using a UUID suffix.
pub fn unique_queue_name(prefix: &str) -> String {
    format!("{}-{}", prefix, uuid::Uuid::new_v4().simple())
}

/// Set up a DeadLetterQueue instance with a test queue.
/// Returns both the DLQ instance and the queue URL.
/// Retries getting the queue URL to handle eventual consistency.
pub async fn setup_dlq_with_queue(
    endpoint_url: &str,
    queue_name: &str,
) -> (crate::sqs::DeadLetterQueue, String) {
    let config = local_config(endpoint_url, None).load().await;
    let client = aws_sdk_sqs::Client::new(&config);

    // Retry getting queue URL - LocalStack may need a moment for the queue to be available
    let queue_url = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut retries = 0;
        loop {
            match client.get_queue_url().queue_name(queue_name).send().await {
                Ok(output) => {
                    if let Some(url) = output.queue_url() {
                        return url.to_string();
                    }
                }
                Err(_) => {
                    retries += 1;
                    if retries >= 10 {
                        panic!("Failed to get queue URL after {} retries", retries);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    })
    .await
    .expect("Timeout waiting for queue to be available");

    let dlq = crate::sqs::DeadLetterQueue {
        config,
        client,
        default_queue_url: Some(queue_url.clone()),
    };

    (dlq, queue_url)
}

/// Create a test queue using the shared container.
pub async fn create_queue_with_shared_container(
    queue_name: &str,
) -> Result<(), TestcontainersError> {
    let container_mutex = get_shared_container().await;
    let container_guard = container_mutex.lock().await;
    create_test_queue(&container_guard.1, queue_name, false).await
}

/// Cleanup the shared container. This should be called after all tests complete.
/// Uses a flag to ensure it only runs once.
pub async fn cleanup_shared_container() {
    if CLEANUP_CALLED.swap(true, Ordering::SeqCst) {
        // Already cleaned up
        return;
    }

    if let Some(container_mutex) = SHARED_CONTAINER.get() {
        let container_guard = container_mutex.lock().await;
        // Stop the container - this will remove it from Docker
        if let Err(e) = container_guard.1.stop().await {
            eprintln!("Warning: Failed to stop shared container: {:?}", e);
        }
    }
}

pub async fn setup(name: &str) -> (crate::DeadLetterQueue, String, aws_config::SdkConfig) {
    let endpoint_url = get_shared_localstack().await;
    let queue_name = unique_queue_name(name);

    create_queue_with_shared_container(&queue_name)
        .await
        .unwrap();

    let (dlq, queue_url) = setup_dlq_with_queue(&endpoint_url, &queue_name).await;

    let config = local_config(&endpoint_url, None).load().await;

    (dlq, queue_url, config)
}

use std::sync::Once;

static RUN_EPILOGUE: Once = Once::new();

// Your cleanup / reporting function
fn run_after_all_tests() {
    println!("\nAll tests finished — running epilogue!");

    if CLEANUP_CALLED.swap(true, Ordering::SeqCst) {
        // Already cleaned up
        return;
    }

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            if let Some(container_mutex) = SHARED_CONTAINER.get() {
                let container_guard = container_mutex.lock().await;
                // Stop the container - this will remove it from Docker
                if let Err(e) = container_guard.1.stop().await {
                    eprintln!("Warning: Failed to stop shared container: {:?}", e);
                }
            }
        });
}

// This struct will run exactly once when dropped (at program exit)
struct Epilogue;
impl Drop for Epilogue {
    fn drop(&mut self) {
        RUN_EPILOGUE.call_once(run_after_all_tests);
    }
}

// Register the guard — this runs immediately when the module loads
static _GUARD: Epilogue = Epilogue;

// Optional: also run on panic (in case process aborts early)
/* #[ctor::ctor]
fn register_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        original_hook(panic_info);
        run_after_all_tests(); // Still try to run
    }));
} */
