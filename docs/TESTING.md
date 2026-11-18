# Testing

This project uses `cargo-nextest` for running tests.

## Commands

```bash
# Install
cargo install cargo-nextest --locked

# Prefer --release so test builds do not fill target/debug
# All tests (~6s with Docker; each SQS test starts its own ElasticMQ container)
cargo nextest run --release

# Faster: share one SQS server across all tests (~1s)
docker compose up -d
DLQ_TEST_SQS_ENDPOINT=http://localhost:4566 cargo nextest run --release

# Doctests (nextest doesn't support these)
cargo test --release --doc

# Specific test / package / target
cargo nextest run --release test_name
cargo nextest run --release -p dlq-core
cargo nextest run --release --test cli

# With output
cargo nextest run --release --no-capture
```

## SQS Tests

Tests that talk to SQS use `SqsTestEnv` from `dlq_core::test_utils` (the `test-utils`
feature). It runs against [ElasticMQ](https://github.com/softwaremill/elasticmq), a
lightweight SQS-compatible server that needs no license or auth token:

- By default, each `SqsTestEnv::start()` starts an `elasticmq-native` container via
  testcontainers. Docker must be running.
- If `DLQ_TEST_SQS_ENDPOINT` is set, tests use that server instead and no containers start.

Every queue gets a unique name, so tests run fully in parallel and can share a server.

## Test Organization

| Type | Location |
|------|----------|
| Unit | `#[cfg(test)]` in `src/*.rs` |
| CLI end-to-end | `crates/dlq-cli/tests/cli.rs` (runs the built `dlq` binary) |
| Property | `crates/dlq-core/tests/send.rs` (`prop_*`) |

## Property Tests

Property tests use `proptest` to check that `send_batch` round-trips arbitrary content.
Each property starts one SQS server and uses a fresh queue per case. Failing cases are
saved in `crates/dlq-core/tests/send.proptest-regressions`; commit that file.

```bash
# More cases than the default 10
PROPTEST_CASES=100 cargo nextest run --release prop_
```

## CI

`.github/workflows/ci.yml` runs fmt, clippy (`-D warnings`), and then:
```bash
cargo nextest run --workspace --all-targets --profile ci
cargo test --workspace --doc
```
