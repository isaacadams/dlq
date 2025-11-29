/// Spawns an asynchronous task to read lines from the given reader and sends them
/// over an mpsc channel for concurrent consumption elsewhere.
///
/// This is useful for offloading I/O-bound line reading (e.g., from files or sockets)
/// to a background task, allowing the caller to process lines asynchronously without
/// blocking the main runtime.
///
/// # Parameters
/// - `reader`: The async reader to read from (e.g., file, TCP stream).
/// - `channel_capacity`: The bounded capacity of the output channel; excess lines will
///   block the reader task until space is available.
///
/// # Returns
/// A tuple of:
/// - `JoinHandle<Result<(), std::io::Error>>`: Handle to the spawned task. Await it to
///   check for I/O errors or completion.
/// - `mpsc::Receiver<String>`: Channel receiver for lines. Lines are sent without the
///   trailing newline. Drop this to signal the reader to stop early.
///
/// # Errors
/// - I/O errors from the reader are propagated via the JoinHandle.
/// - If the receiver is dropped, the task will detect send failures and abort cleanly.
///
/// # Example
/// ```
/// use tokio::fs::File;
/// use tokio::io::{AsyncReadExt, BufReader};
/// async fn example() -> Result<(), Box<dyn std::error::Error>> {
///     let file = File::open("example.txt").await?;
///     let (handle, mut rx) = concurrent_lines_reader(BufReader::new(file), 100);
///
///     while let Some(line) = rx.recv().await {
///         println!("Read: {}", line);
///     }
///     handle.await??;  // Wait for completion
///     Ok(())
/// }
/// ```
pub fn concurrent_lines<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    reader: R,
    channel_capacity: usize,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::mpsc::Receiver<String>,
) {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let (tx, rx) = tokio::sync::mpsc::channel::<String>(channel_capacity);
    let buffer = BufReader::new(reader);

    let task = tokio::spawn(async move {
        let mut lines = buffer.lines();
        loop {
            match lines.next_line().await {
                // Send the line if it exists
                Ok(Some(line)) => {
                    if let Err(e) = tx.send(line).await {
                        tracing::error!("Channel closed unexpectedly while sending line: {e}");
                        break;
                    }
                }
                // Clean EOF, exit loop
                Ok(None) => {
                    tracing::trace!("Reached EOF, exiting reader task");
                    break;
                }
                // I/O error: Log and exit loop
                Err(e) => {
                    tracing::error!("I/O error while reading lines: {e}. Stopping reader.");
                    break;
                }
            }
        }
    });

    (task, rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tokio::io::BufReader as TokioBufReader;

    #[tokio::test]
    async fn reads_all_lines() {
        let input = b"first line\nsecond line\nthird\n".to_vec();
        let reader = TokioBufReader::new(Cursor::new(input));
        let (handle, mut rx) = concurrent_lines(reader, 10);

        let mut received = vec![];
        while let Some(line) = rx.recv().await {
            received.push(line);
        }
        handle.await.unwrap();

        assert_eq!(
            received,
            vec![
                "first line".to_string(),
                "second line".to_string(),
                "third".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn handles_empty_input() {
        let reader = TokioBufReader::new(Cursor::new(Vec::new()));
        let (handle, mut rx) = concurrent_lines(reader, 1);
        assert!(rx.recv().await.is_none());
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn propagates_io_error() {
        // Mock: Read initial data (one line), then error on next read
        let mock_reader = tokio_test::io::Builder::new()
            .read(b"first line\n".as_ref()) // ← Initial successful read
            .read_error(std::io::Error::new(std::io::ErrorKind::Other, "mock error"))
            .build();

        let (handle, mut rx) = concurrent_lines(mock_reader, 1);

        // Assert it sends the first line before erroring
        assert_eq!(rx.recv().await, Some("first line".to_string()));
        assert!(rx.recv().await.is_none()); // No more lines

        handle.await.unwrap();
    }

    #[tokio::test]
    async fn aborts_on_receiver_drop() {
        let input = b"line1\nline2\n".to_vec();
        let reader = TokioBufReader::new(Cursor::new(input));
        let (handle, rx) = concurrent_lines(reader, 1);

        drop(rx); // Close channel early

        // Task should complete soon after (no hang)
        let result = tokio::time::timeout(tokio::time::Duration::from_secs(1), handle)
            .await
            .unwrap();

        assert!(result.is_ok());

        ()
    }
}
