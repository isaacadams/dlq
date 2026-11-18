//! Property-based round-trip tests for `send_batch` against a local SQS server.
//!
//! Each property starts one runtime and one SQS server, then runs every case against a
//! fresh queue on that server, so cases are isolated without paying container startup
//! per case.

use std::collections::HashSet;

use dlq::test_utils::SqsTestEnv;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};

/// Default case count; override with `PROPTEST_CASES`.
const CASES: u32 = 10;

/// Runs `strategy` for `CASES` cases, asserting each generated batch round-trips intact.
fn check_roundtrip(strategy: impl Strategy<Value = Vec<String>>) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let env = rt.block_on(SqsTestEnv::start());

    let mut runner = TestRunner::new(Config {
        cases: std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(CASES),
        source_file: Some(file!()),
        ..Config::default()
    });
    let result = runner.run(&strategy, |messages| rt.block_on(roundtrip(&env, messages)));

    // Drop the container inside the runtime so it is removed cleanly.
    rt.block_on(async move { drop(env) });
    if let Err(e) = result {
        panic!("{e}\n{runner}");
    }
}

async fn roundtrip(env: &SqsTestEnv, messages: Vec<String>) -> Result<(), TestCaseError> {
    let queue_url = env.create_queue("test-prop").await;

    let result = env.dlq().send_batch(&queue_url, &messages).await;
    prop_assert!(result.is_ok(), "send_batch failed: {:?}", result);

    let received = env.receive_bodies(&queue_url, messages.len()).await;
    prop_assert_eq!(received.len(), messages.len(), "message count changed");

    let received: HashSet<_> = received.into_iter().collect();
    let sent: HashSet<_> = messages.into_iter().collect();
    prop_assert_eq!(received, sent, "message content changed");
    Ok(())
}

#[test]
fn prop_send_batch_roundtrip() {
    // Alphanumeric + spaces, 1-9 messages (SQS batch limit is 10)
    check_roundtrip(prop::collection::vec("[a-zA-Z0-9 ]{1,1000}", 1..10usize));
}

#[test]
fn prop_send_unicode_content() {
    check_roundtrip(prop::collection::vec("\\PC{1,500}", 1..5usize));
}

#[test]
fn prop_send_various_batch_sizes() {
    // Messages with unique suffixes, so every batch size is fully distinguishable
    check_roundtrip(
        (1..10usize, "[a-z]{10,50}").prop_map(|(batch_size, template)| {
            (0..batch_size).map(|i| format!("{template}-{i}")).collect()
        }),
    );
}

#[test]
fn prop_send_special_content() {
    // Special chars, whitespace, JSON-like
    let special = prop::string::string_regex(r#"[ \t\n\r\{\}\[\]"'\\]{1,200}"#).unwrap();
    check_roundtrip(prop::collection::vec(special, 1..5usize));
}
