use crate::database::Database;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub async fn run() {
    let (h_stdin, rx_stdin) = crate::reader::concurrent_lines(tokio::io::stdin(), 100);

    SqsBatch::local(
        "http://sqs.us-east-1.localhost.localstack.cloud:4566/000000000000/demo",
        Some("http://localhost:4566"),
    )
    .await
    .send(rx_stdin)
    .await;

    if let Err(e) = h_stdin.await {
        eprintln!("error from stdin: {e}");
    }
}

/// Represents a job item with its database ID for tracking results
#[derive(Debug, Clone)]
struct JobItem {
    db_id: i64,
    input: Value,
}

/// Result of sending an item to SQS
#[derive(Debug, Clone)]
struct SendResult {
    db_id: i64,
    success: bool,
    output: Option<Value>,
    error: Option<String>,
    #[allow(dead_code)]
    batch_id: String,
}

/// Verify that the given queue URL exists, or return a helpful error with available queues
async fn verify_queue_exists(client: &aws_sdk_sqs::Client, queue_url: &str) -> anyhow::Result<()> {
    // Try to get queue attributes to verify it exists
    match client
        .get_queue_attributes()
        .queue_url(queue_url)
        .attribute_names(aws_sdk_sqs::types::QueueAttributeName::QueueArn)
        .send()
        .await
    {
        Ok(_) => Ok(()),
        Err(e) => {
            // Check if it's a queue not found error
            let is_not_found = if let aws_sdk_sqs::error::SdkError::ServiceError(se) = &e {
                let err_str = format!("{}", se.err());
                err_str.contains("NonExistentQueue")
                    || err_str.contains("QueueDoesNotExist")
                    || err_str.contains("does not exist")
            } else {
                false
            };

            if is_not_found {
                // List available queues to help the user
                let mut available_queues = Vec::new();
                let mut list_result = client.list_queues().send().await;

                while let Ok(output) = list_result {
                    if let Some(urls) = output.queue_urls {
                        available_queues.extend(urls);
                    }
                    match output.next_token {
                        Some(token) => {
                            list_result = client.list_queues().next_token(token).send().await;
                        }
                        None => break,
                    }
                }

                let mut error_msg = format!("Queue not found: {}\n\n", queue_url);

                if available_queues.is_empty() {
                    error_msg.push_str("No queues are currently available.\n\n");
                    error_msg.push_str("To create a new queue, you can use the AWS CLI:\n");
                    error_msg.push_str("  aws sqs create-queue --queue-name <your-queue-name>\n\n");
                    error_msg.push_str("Or with LocalStack:\n");
                    error_msg.push_str("  aws --endpoint-url=http://localhost:4566 sqs create-queue --queue-name <your-queue-name>");
                } else {
                    error_msg.push_str("Available queues:\n");
                    for (i, url) in available_queues.iter().enumerate() {
                        // Extract queue name from URL for cleaner display
                        let name = url.rsplit('/').next().unwrap_or(url);
                        error_msg.push_str(&format!("  {}. {} \n     {}\n", i + 1, name, url));
                    }
                    error_msg.push_str("\nTo create a new queue, use the AWS CLI:\n");
                    error_msg.push_str("  aws sqs create-queue --queue-name <your-queue-name>");
                }

                Err(anyhow::anyhow!("{}", error_msg))
            } else {
                // Some other error occurred
                Err(anyhow::anyhow!("Failed to verify queue: {:?}", e))
            }
        }
    }
}

/// Run batch send from database with streaming architecture
pub async fn run_batch(
    job_id: i64,
    queue_url: &str,
    endpoint: Option<&str>,
    batch_size: usize,
    stage_size: Option<i64>,
    concurrency: usize,
    _retry_limit: u32,
) -> anyhow::Result<()> {
    let db = Arc::new(Database::new().await?);

    // Verify job exists and has the correct name
    let job = db
        .get_job(job_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Job {} not found", job_id))?;

    if job.name != "send_batch" {
        return Err(anyhow::anyhow!(
            "Job {} has name '{}', expected 'send_batch'",
            job_id,
            job.name
        ));
    }

    // Create SQS client early to verify queue exists
    let sqs = SqsBatch::local(queue_url, endpoint).await;
    let client = Arc::new(aws_sdk_sqs::Client::new(&sqs.aws_config));

    // Verify the queue exists before processing
    verify_queue_exists(&client, queue_url).await?;

    let config = job.configuration.0;
    let input_type = Arc::new(config.input_type.clone());

    // Count total pending items for progress bar
    let total_pending = db.count_pending_items(job_id).await?;
    if total_pending == 0 {
        println!("No pending items to process for job {}", job_id);
        return Ok(());
    }

    // Auto-optimize stage size if not provided
    let stage_size = stage_size.unwrap_or({
        if total_pending < 1000 {
            total_pending
        } else {
            1000
        }
    });

    println!(
        "Processing job {} ({} pending items, stage_size={}, batch_size={}, concurrency={})\n",
        job_id, total_pending, stage_size, batch_size, concurrency
    );

    // Setup multi-progress bars
    let mp = MultiProgress::new();

    // Loading progress bar (items staged from SQLite)
    let load_style = ProgressStyle::default_bar()
        .template("{prefix:.bold.dim} {spinner:.green} [{bar:30.white/dim}] {pos}/{len} staged")
        .unwrap()
        .progress_chars("━━╸");

    let load_pb = mp.add(ProgressBar::new(total_pending as u64));
    load_pb.set_style(load_style);
    load_pb.set_prefix("SQLite");

    // Create progress bars for each concurrent sender
    let sender_style = ProgressStyle::default_bar()
        .template("{prefix:.bold.cyan} {spinner:.blue} [{bar:30.cyan/dim}] {pos} batches sent")
        .unwrap()
        .progress_chars("━━╸");

    let sender_pbs: Vec<ProgressBar> = (0..concurrency)
        .map(|i| {
            let pb = mp.add(ProgressBar::new_spinner());
            pb.set_style(sender_style.clone());
            pb.set_prefix(format!("Worker {}", i + 1));
            pb.enable_steady_tick(std::time::Duration::from_millis(100));
            pb
        })
        .collect();
    let sender_pbs = Arc::new(sender_pbs);

    // Completed items progress bar (total items written back to SQLite)
    let complete_style = ProgressStyle::default_bar()
        .template("{prefix:.bold.green} {spinner:.green} [{bar:30.green/dim}] {pos}/{len} completed ({percent}%)")
        .unwrap()
        .progress_chars("━━╸");

    let complete_pb = mp.add(ProgressBar::new(total_pending as u64));
    complete_pb.set_style(complete_style);
    complete_pb.set_prefix("Total");
    complete_pb.enable_steady_tick(std::time::Duration::from_millis(100));

    // Channel for streaming items from loader to senders
    // Buffer size balances memory with throughput
    let (item_tx, item_rx) = async_channel::bounded::<JobItem>(batch_size * concurrency * 2);

    // Channel for results back to the database writer
    let (result_tx, mut result_rx) = tokio::sync::mpsc::channel::<SendResult>(1000);

    // Channel for database operations - ALL writes go through this single task
    enum DbOp {
        StageRequest {
            response_tx: tokio::sync::oneshot::Sender<Result<Vec<(i64, Value)>, String>>,
        },
    }
    let (db_tx, mut db_rx) = tokio::sync::mpsc::channel::<DbOp>(100);

    // Shared counters
    let items_loaded = Arc::new(AtomicU64::new(0));
    let queue_url = Arc::new(queue_url.to_string());

    // Buffer threshold for batch writes
    const WRITE_BUFFER_SIZE: usize = 100;

    // Spawn single database task that handles ALL writes
    // This serializes staging + completions to avoid SQLite lock contention
    let writer_db = db.clone();
    let complete_pb_clone = complete_pb.clone();
    let db_writer_handle = tokio::spawn(async move {
        use crate::database::CompletionResult;

        let mut completion_buffer: Vec<SendResult> = Vec::with_capacity(WRITE_BUFFER_SIZE);
        let mut total_completed: u64 = 0;

        loop {
            tokio::select! {
                // Handle staging requests (priority)
                Some(DbOp::StageRequest { response_tx }) = db_rx.recv() => {
                    // Flush any pending completions first to keep order
                    if !completion_buffer.is_empty() {
                        let count = completion_buffer.len();
                        let results: Vec<CompletionResult> = completion_buffer
                            .drain(..)
                            .map(|r| CompletionResult {
                                id: r.db_id,
                                output: r.output,
                                error: if r.success { None } else { r.error },
                                success: r.success,
                            })
                            .collect();

                        if let Err(e) = writer_db.complete_items_batch(results).await {
                            eprintln!("Error batch completing items: {}", e);
                        } else {
                            total_completed += count as u64;
                            complete_pb_clone.set_position(total_completed);
                        }
                    }

                    let result = writer_db
                        .stage_pending_items(Some(job_id), stage_size)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = response_tx.send(result);
                }
                // Collect completion results
                Some(result) = result_rx.recv() => {
                    completion_buffer.push(result);

                    // Flush if buffer is full
                    if completion_buffer.len() >= WRITE_BUFFER_SIZE {
                        let count = completion_buffer.len();
                        let results: Vec<CompletionResult> = completion_buffer
                            .drain(..)
                            .map(|r| CompletionResult {
                                id: r.db_id,
                                output: r.output,
                                error: if r.success { None } else { r.error },
                                success: r.success,
                            })
                            .collect();

                        if let Err(e) = writer_db.complete_items_batch(results).await {
                            eprintln!("Error batch completing items: {}", e);
                        } else {
                            total_completed += count as u64;
                            complete_pb_clone.set_position(total_completed);
                        }
                    }
                }
                // Both channels closed - flush and exit
                else => {
                    if !completion_buffer.is_empty() {
                        let count = completion_buffer.len();
                        let results: Vec<CompletionResult> = completion_buffer
                            .drain(..)
                            .map(|r| CompletionResult {
                                id: r.db_id,
                                output: r.output,
                                error: if r.success { None } else { r.error },
                                success: r.success,
                            })
                            .collect();

                        if let Err(e) = writer_db.complete_items_batch(results).await {
                            eprintln!("Error batch completing items: {}", e);
                        } else {
                            total_completed += count as u64;
                            complete_pb_clone.set_position(total_completed);
                        }
                    }
                    complete_pb_clone.finish();
                    break;
                }
            }
        }
    });

    // Spawn the loader task - requests staging through the db writer
    let loader_db_tx = db_tx.clone();
    let load_pb_clone = load_pb.clone();
    let items_loaded_clone = items_loaded.clone();
    let loader_handle = tokio::spawn(async move {
        'loader: loop {
            // Request staging through the serialized db writer
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            if loader_db_tx
                .send(DbOp::StageRequest { response_tx })
                .await
                .is_err()
            {
                break 'loader;
            }

            let items = match response_rx.await {
                Ok(Ok(items)) => items,
                Ok(Err(e)) => {
                    eprintln!("Error staging items: {}", e);
                    break 'loader;
                }
                Err(_) => break 'loader,
            };

            if items.is_empty() {
                break 'loader;
            }

            for (db_id, input) in items {
                let item = JobItem { db_id, input };
                if item_tx.send(item).await.is_err() {
                    // Receiver dropped, stop loading entirely
                    break 'loader;
                }
                let loaded = items_loaded_clone.fetch_add(1, Ordering::Relaxed) + 1;
                load_pb_clone.set_position(loaded);
            }
        }

        load_pb_clone.finish_with_message("done");
        // Close the channel to signal completion
        drop(item_tx);
    });

    // Spawn sender worker tasks
    let mut sender_handles = Vec::new();
    for worker_id in 0..concurrency {
        let client = client.clone();
        let queue_url = queue_url.clone();
        let input_type = input_type.clone();
        let item_rx = item_rx.clone();
        let result_tx = result_tx.clone();
        let pb = sender_pbs[worker_id].clone();

        let handle = tokio::spawn(async move {
            let mut batches_sent = 0u64;

            loop {
                // Collect items for a batch
                let mut batch_items = Vec::with_capacity(batch_size);

                // Try to fill the batch
                for _ in 0..batch_size {
                    match item_rx.recv().await {
                        Ok(item) => batch_items.push(item),
                        Err(_) => break, // Channel closed
                    }
                }

                if batch_items.is_empty() {
                    break; // No more items
                }

                // Transform and send batch
                let batch_id = uuid::Uuid::new_v4().hyphenated().to_string();
                let mut entries = Vec::with_capacity(batch_items.len());
                let mut id_to_db_id = HashMap::new();

                for item in &batch_items {
                    let (message_id, message_body) = transform_input(&item.input, &input_type);
                    id_to_db_id.insert(message_id.clone(), item.db_id);

                    if let Ok(entry) = aws_sdk_sqs::types::SendMessageBatchRequestEntry::builder()
                        .id(&message_id)
                        .message_body(message_body)
                        .build()
                    {
                        entries.push((message_id, entry));
                    }
                }

                if entries.is_empty() {
                    continue;
                }

                let entry_list: Vec<_> = entries.iter().map(|(_, e)| e.clone()).collect();
                let entry_ids: Vec<_> = entries.iter().map(|(id, _)| id.clone()).collect();

                // Send to SQS
                match client
                    .send_message_batch()
                    .queue_url(queue_url.as_ref())
                    .set_entries(Some(entry_list))
                    .send()
                    .await
                {
                    Ok(output) => {
                        // Process successful entries
                        for success in output.successful() {
                            if let Some(&db_id) = id_to_db_id.get(success.id()) {
                                let _ = result_tx
                                    .send(SendResult {
                                        db_id,
                                        success: true,
                                        output: Some(serde_json::json!({
                                            "message_id": success.message_id(),
                                            "md5_of_message_body": success.md5_of_message_body(),
                                            "sequence_number": success.sequence_number(),
                                            "batch_id": batch_id,
                                        })),
                                        error: None,
                                        batch_id: batch_id.clone(),
                                    })
                                    .await;
                            }
                        }

                        // Process failed entries
                        for failed in output.failed() {
                            if let Some(&db_id) = id_to_db_id.get(failed.id()) {
                                let _ = result_tx
                                    .send(SendResult {
                                        db_id,
                                        success: false,
                                        output: Some(serde_json::json!({
                                            "code": failed.code(),
                                            "message": failed.message(),
                                            "sender_fault": failed.sender_fault(),
                                            "batch_id": batch_id,
                                        })),
                                        error: Some(format!(
                                            "{}: {}",
                                            failed.code(),
                                            failed.message().unwrap_or("Unknown error")
                                        )),
                                        batch_id: batch_id.clone(),
                                    })
                                    .await;
                            }
                        }
                    }
                    Err(e) => {
                        let (error_msg, error_details) = format_sdk_error(&e);
                        for entry_id in entry_ids {
                            if let Some(&db_id) = id_to_db_id.get(&entry_id) {
                                let _ = result_tx
                                    .send(SendResult {
                                        db_id,
                                        success: false,
                                        output: Some(serde_json::json!({
                                            "error_details": error_details,
                                            "batch_id": batch_id,
                                        })),
                                        error: Some(error_msg.clone()),
                                        batch_id: batch_id.clone(),
                                    })
                                    .await;
                            }
                        }
                    }
                }

                batches_sent += 1;
                pb.set_position(batches_sent);
            }

            pb.finish_with_message("done");
        });

        sender_handles.push(handle);
    }

    // Drop our copies of channels so they close when tasks are done
    drop(result_tx);
    drop(db_tx);

    // Wait for loader to finish
    let _ = loader_handle.await;

    // Wait for all senders to finish
    for handle in sender_handles {
        let _ = handle.await;
    }

    // Wait for db writer to finish (it will exit when both channels close)
    let _ = db_writer_handle.await;

    // Clear intermediate progress bars, keep the completion bar
    load_pb.finish_and_clear();
    for pb in sender_pbs.iter() {
        pb.finish_and_clear();
    }

    // The complete_pb was already finished in the db_writer task
    // Just add a newline for clean output
    println!();

    // Check and close job if complete
    match db.check_and_close_job(job_id).await? {
        None => println!("✓ Job {} completed successfully", job_id),
        Some(remaining) => println!(
            "Job {} still has {} items pending/processing",
            job_id, remaining
        ),
    }

    Ok(())
}

/// Format an SDK error into a detailed error message and JSON details
fn format_sdk_error(
    e: &aws_sdk_sqs::error::SdkError<
        aws_sdk_sqs::operation::send_message_batch::SendMessageBatchError,
    >,
) -> (String, Value) {
    use aws_sdk_sqs::error::SdkError;

    match e {
        SdkError::ServiceError(se) => {
            let err = se.err();
            let raw = se.raw();

            let error_msg = format!("SQS ServiceError: {}", err);
            let details = serde_json::json!({
                "error_type": "ServiceError",
                "message": format!("{}", err),
                "status_code": raw.status().as_u16(),
                "request_id": raw.headers()
                    .get("x-amzn-requestid")
                    .map(|v| v.to_string()),
            });
            (error_msg, details)
        }
        SdkError::TimeoutError(te) => {
            let error_msg = format!("SQS TimeoutError: {:?}", te);
            let details = serde_json::json!({
                "error_type": "TimeoutError",
                "message": format!("{:?}", te),
            });
            (error_msg, details)
        }
        SdkError::DispatchFailure(df) => {
            let error_msg = format!("SQS DispatchFailure: {:?}", df);
            let details = serde_json::json!({
                "error_type": "DispatchFailure",
                "message": format!("{:?}", df),
                "is_io": df.is_io(),
                "is_timeout": df.is_timeout(),
                "is_user": df.is_user(),
                "connector_error": df.as_connector_error().map(|ce| format!("{:?}", ce)),
            });
            (error_msg, details)
        }
        SdkError::ResponseError(re) => {
            let raw = re.raw();
            let error_msg = format!("SQS ResponseError: status={}", raw.status().as_u16());
            let details = serde_json::json!({
                "error_type": "ResponseError",
                "status_code": raw.status().as_u16(),
                "debug": format!("{:?}", re),
            });
            (error_msg, details)
        }
        SdkError::ConstructionFailure(cf) => {
            let error_msg = format!("SQS ConstructionFailure: {:?}", cf);
            let details = serde_json::json!({
                "error_type": "ConstructionFailure",
                "message": format!("{:?}", cf),
            });
            (error_msg, details)
        }
        _ => {
            let error_msg = format!("SQS Error: {:?}", e);
            let details = serde_json::json!({
                "error_type": "Unknown",
                "debug": format!("{:?}", e),
            });
            (error_msg, details)
        }
    }
}

/// Transform input value to (message_id, message_body) based on input_type
fn transform_input(input: &Value, input_type: &str) -> (String, String) {
    match input_type {
        "json" => {
            // JSON mode: extract id and body fields
            if let Value::Object(map) = input {
                let id = map
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| uuid::Uuid::new_v4().hyphenated().to_string());

                let body = map
                    .get("body")
                    .map(|v| {
                        if let Value::String(s) = v {
                            s.clone()
                        } else {
                            serde_json::to_string(v).unwrap_or_default()
                        }
                    })
                    .unwrap_or_else(|| serde_json::to_string(input).unwrap_or_default());

                (id, body)
            } else {
                // Fallback: treat as text
                let id = uuid::Uuid::new_v4().hyphenated().to_string();
                let body = serde_json::to_string(input).unwrap_or_default();
                (id, body)
            }
        }
        _ => {
            // TEXT mode (default): input is raw text, generate UUID for message ID
            let id = uuid::Uuid::new_v4().hyphenated().to_string();
            let body = match input {
                Value::String(s) => s.clone(),
                _ => serde_json::to_string(input).unwrap_or_default(),
            };
            (id, body)
        }
    }
}

pub struct SqsBatch {
    pub aws_config: aws_config::SdkConfig,
    pub aws_sqs_queue_url: String,
}

impl SqsBatch {
    pub async fn local(aws_sqs_queue_url: &str, aws_endpoint: Option<impl Into<String>>) -> Self {
        let endpoint = aws_endpoint
            .map(|s| s.into())
            .or(std::env::var("AWS_ENDPOINT").ok())
            .or(Some("http://localhost:4566".into()));

        let mut loader = aws_config::from_env()
            .region(
                // supports loading region from known env variables
                aws_config::meta::region::RegionProviderChain::default_provider()
                    .or_else(aws_config::Region::from_static("us-east-1")),
            )
            .credentials_provider(aws_sdk_sqs::config::Credentials::new(
                "test", "test", None, None, "static",
            ));

        if let Some(endpoint) = endpoint {
            loader = loader.endpoint_url(endpoint);
        }

        Self {
            aws_config: loader.load().await,
            aws_sqs_queue_url: aws_sqs_queue_url.to_string(),
        }
    }

    pub async fn send(&self, rx: tokio::sync::mpsc::Receiver<String>) {
        let client = std::sync::Arc::new(aws_sdk_sqs::Client::new(&self.aws_config));
        let aws_sqs_queue_url = std::sync::Arc::new(self.aws_sqs_queue_url.clone());

        use pumps::{Concurrency, Pipeline};
        let (mut rx_pipeline, h_pipeline) = Pipeline::from(rx)
            .filter_map(
                |x| async move {
                    if x.is_empty() {
                        return None;
                    }

                    println!("{}", x);

                    // BatchId is a wrapper around a string that validates the ID is a valid SQS batch ID
                    // BatchId::new(uuid::Uuid::new_v4().to_string()).unwrap().0
                    let id = uuid::Uuid::new_v4().hyphenated().to_string();
                    let entry = aws_sdk_sqs::types::SendMessageBatchRequestEntry::builder()
                        .id(id)
                        .message_body(x)
                        .build()
                        .unwrap();

                    println!("entry: {:#?}", entry);
                    Some(entry)
                },
                Concurrency::serial(),
            )
            .batch(10)
            .map(
                move |entries| {
                    let client = client.clone();
                    let aws_sqs_queue_url = aws_sqs_queue_url.clone();
                    async move {
                        job(&client, aws_sqs_queue_url.as_ref(), entries).await;
                    }
                },
                // SQS batch sends are inherently unordered in results (failed items can come back in any order)
                // using unordered concurrency gives better throughput
                Concurrency::concurrent_unordered(10),
            )
            .build();

        while rx_pipeline.recv().await.is_some() {}

        if let Err(e) = h_pipeline.await {
            eprintln!("error from pipeline: {e}");
        }
    }
}

async fn job(
    client: &aws_sdk_sqs::Client,
    queue_url: &str,
    entries: Vec<aws_sdk_sqs::types::SendMessageBatchRequestEntry>,
) {
    match client
        .send_message_batch()
        .queue_url(queue_url)
        .set_entries(Some(entries))
        .send()
        .await
    {
        Ok(output) => {
            println!("output: {:#?}", output);
        }
        Err(e) => {
            if let aws_sdk_sqs::error::SdkError::ServiceError(se) = &e {
                let err = se.err();
                // This prints the primary AWS service error message (e.g., from the XML/JSON response)
                eprintln!("[AWS SDK ERROR] {}", err);
            } else {
                // Fallback for non-service errors (e.g., timeout, dispatch failure)
                eprintln!("[AWS SDK ERROR] {}", e);
            }
        }
    }
}

#[allow(dead_code)]
mod validation {
    use std::convert::Into;

    #[derive(Debug, Clone)]
    pub struct BatchId(String);

    impl BatchId {
        pub fn new<S: Into<String>>(id: S) -> Result<Self, String> {
            let id_str = id.into();
            if id_str.is_empty() {
                return Err("Batch ID cannot be empty".to_string());
            }
            if id_str.len() > 80 {
                return Err(format!(
                    "Batch ID exceeds maximum length: {} > 80 characters",
                    id_str.len()
                ));
            }
            for c in id_str.chars() {
                if !c.is_alphanumeric() && c != '-' && c != '_' {
                    return Err(format!(
                        "Invalid character in Batch ID: '{}'. Allowed: alphanumeric, '-', '_'",
                        c
                    ));
                }
            }
            Ok(Self(id_str))
        }
    }

    impl AsRef<str> for BatchId {
        fn as_ref(&self) -> &str {
            &self.0
        }
    }
}

#[cfg(test)]
mod test {
    use crate::reader::concurrent_lines;
    use crate::test::{create_test_queue, localstack};

    use super::*;

    #[tokio::test]
    async fn test_local() {
        // create a localstack container and a queue
        let (endpoint, container) = localstack().await.unwrap();
        let queue_url = create_test_queue(&container, "batch-send-test", false)
            .await
            .unwrap();

        // create a reader for the batch messages
        let reader = tokio::io::BufReader::new(std::io::Cursor::new(
            b"first line\nsecond line\nthird\n".to_vec(),
        ));
        let (handle, rx) = concurrent_lines(reader, 10);

        // create a sqs batch client and send the batch messages to the queue
        let sqs = SqsBatch::local(&queue_url, Some(endpoint)).await;
        sqs.send(rx).await;

        // wait for the handle to complete to ensure the messages are sent
        handle.await.unwrap();

        // create a sqs client and receive the messages from the queue
        let client = aws_sdk_sqs::Client::new(&sqs.aws_config);
        let receive_output = client
            .receive_message()
            .queue_url(&queue_url)
            .max_number_of_messages(10)
            .send()
            .await
            .unwrap();

        // verify the messages were received without respect to the order
        let messages = receive_output.messages.unwrap_or_default();
        assert_eq!(messages.len(), 3);

        let received_bodies = messages
            .iter()
            .filter_map(|m| m.body())
            .collect::<Vec<&str>>();

        assert_eq!(received_bodies.len(), 3);
        assert!(received_bodies.contains(&"first line"));
        assert!(received_bodies.contains(&"second line"));
        assert!(received_bodies.contains(&"third"));

        container.stop().await.unwrap();

        ()
    }
}
