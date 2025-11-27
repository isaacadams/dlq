mod common;

use common::create_or_get_queue_url;
use proptest::prelude::*;
use std::collections::HashSet;
// Property-based test: Send-receive symmetry
proptest! {
    #![proptest_config(ProptestConfig::with_cases(10))]

    #[test]
    fn prop_send_batch_roundtrip(
        messages in prop::collection::vec(
            "[a-zA-Z0-9 ]{1,1000}",  // Alphanumeric + spaces, 1-1000 chars
            1..10usize  // 1-9 messages (SQS batch limit is 10)
        )
    ) {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let queue = create_or_get_queue_url("test-prop-roundtrip");
            let dlq = dlq::DeadLetterQueue::new(Some(aws_sdk_sqs::config::Credentials::new("test", "test", None, None, "static")), Some("http://localhost:4566"), Some(&queue)).await;

            // Send the batch
            let result = dlq.send_batch(&messages).await;
            prop_assert!(result.is_ok(), "send_batch should succeed for valid input: {:?}", result);

            // Receive and verify count
            let receive_output = dlq
                .client
                .receive_message()
                .queue_url(dlq.default_queue_url.as_ref().unwrap())
                .max_number_of_messages(10)
                .send()
                .await
                .unwrap();

            let received = receive_output.messages.unwrap_or_default();
            prop_assert_eq!(
                received.len(),
                messages.len(),
                "Should receive same number of messages as sent"
            );

            // Verify all messages received (order independent)
            let received_bodies: HashSet<_> =
                received.iter()
                .map(|m| m.body().unwrap_or("").to_string())
                .collect();

            let sent_bodies: HashSet<_> =
                messages.iter()
                .map(|s| s.to_string())
                .collect();

            prop_assert_eq!(received_bodies, sent_bodies, "Message content should be preserved");

            Ok(())
        })?;
    }
}

// Property-based test: Unicode and special characters
proptest! {
    #![proptest_config(ProptestConfig::with_cases(10))]

    #[test]
    fn prop_send_unicode_content(
        messages in prop::collection::vec(
            "\\PC{1,500}",  // Any unicode character, 1-500 chars
            1..5usize  // 1-4 messages
        )
    ) {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let queue = create_or_get_queue_url("test-prop-unicode");
            let dlq = dlq::DeadLetterQueue::new(Some(aws_sdk_sqs::config::Credentials::new("test", "test", None, None, "static")), Some("http://localhost:4566"), Some(&queue)).await;

            let result = dlq.send_batch(&messages).await;
            prop_assert!(result.is_ok(), "send_batch should handle unicode content");

            // Verify content preservation
            let receive_output = dlq
                .client
                .receive_message()
                .queue_url(dlq.default_queue_url.as_ref().unwrap())
                .max_number_of_messages(10)
                .send()
                .await
                .unwrap();

            let received = receive_output.messages.unwrap_or_default();
            prop_assert_eq!(received.len(), messages.len(), "Should preserve message count");

            // Verify content preservation
            let received_bodies: HashSet<_> =
                received.iter()
                .map(|m| m.body().unwrap_or("").to_string())
                .collect();

            let sent_bodies: HashSet<_> =
                messages.iter()
                .map(|s| s.to_string())
                .collect();

            prop_assert_eq!(received_bodies, sent_bodies, "Unicode content should be preserved exactly");

            Ok(())
        })?;
    }
}

// Property-based test: Various batch sizes
proptest! {
    #![proptest_config(ProptestConfig::with_cases(10))]

    #[test]
    fn prop_send_various_batch_sizes(
        batch_size in 1..10usize,
        message_template in "[a-z]{10,50}"
    ) {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let queue = create_or_get_queue_url("test-prop-sizes");
            let dlq = dlq::DeadLetterQueue::new(Some(aws_sdk_sqs::config::Credentials::new("test", "test", None, None, "static")), Some("http://localhost:4566"), Some(&queue)).await;

            // Generate messages with unique suffixes
            let messages: Vec<String> = (0..batch_size)
                .map(|i| format!("{}-{}", message_template, i))
                .collect();

            let result = dlq.send_batch(&messages).await;
            prop_assert!(result.is_ok(), "send_batch should handle batch size {}", batch_size);

            // Verify all messages received
            let receive_output = dlq
                .client
                .receive_message()
                .queue_url(dlq.default_queue_url.as_ref().unwrap())
                .max_number_of_messages(10)
                .send()
                .await
                .unwrap();

            let received = receive_output.messages.unwrap_or_default();
            prop_assert_eq!(received.len(), batch_size, "Should receive all {} messages", batch_size);

            Ok(())
        })?;
    }
}

// Property-based test: Edge cases with special strings
proptest! {
    #![proptest_config(ProptestConfig::with_cases(10))]

    #[test]
    fn prop_send_special_content(
        messages in prop::collection::vec(
            prop::string::string_regex(
                r#"[ \t\n\r\{\}\[\]"'\\]{1,200}"#  // Special chars, whitespace, JSON-like
            ).unwrap(),
            1..5usize
        ).prop_filter("Filter out empty messages", |msgs| msgs.iter().all(|m| !m.is_empty()))
    ) {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let queue = create_or_get_queue_url("test-prop-special");
            let dlq = dlq::DeadLetterQueue::new(Some(aws_sdk_sqs::config::Credentials::new("test", "test", None, None, "static")), Some("http://localhost:4566"), Some(&queue)).await;

            let result = dlq.send_batch(&messages).await;
            prop_assert!(result.is_ok(), "send_batch should handle special characters");

            let receive_output = dlq
                .client
                .receive_message()
                .queue_url(dlq.default_queue_url.as_deref().unwrap())
                .max_number_of_messages(10)
                .send()
                .await
                .unwrap();

            let received = receive_output.messages.unwrap_or_default();
            prop_assert_eq!(received.len(), messages.len());

            // Verify exact content preservation
            let received_bodies: HashSet<_> =
                received.iter()
                .map(|m| m.body().unwrap_or("").to_string())
                .collect();

            let sent_bodies: HashSet<_> =
                messages.iter()
                .map(|s| s.to_string())
                .collect();

            prop_assert_eq!(received_bodies, sent_bodies, "Special characters should be preserved");

            Ok(())
        })?;
    }
}
