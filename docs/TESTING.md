# Testing

This project uses `cargo-nextest` for running tests.

## Commands

```bash
# Install
cargo install cargo-nextest --locked

# Unit tests (fast, run frequently)
cargo nextest run --lib

# Integration tests (slower, uses LocalStack)
cargo nextest run --test '*'

# Property tests (slowest)
cargo nextest run --features prop-tests

# All tests
cargo nextest run --workspace

# Doctests (nextest doesn't support these)
cargo test --doc

# Specific test
cargo nextest run test_name

# With output
cargo nextest run --nocapture

# With logging
RUST_LOG=debug cargo nextest run test_name
```

## Test Organization

| Type | Location | Run With |
|------|----------|----------|
| Unit | `#[cfg(test)]` in `src/*.rs` | `--lib` |
| Integration | `tests/*.rs` | `--test '*'` |
| Property | Feature-gated in source | `--features prop-tests` |

## Property Tests

Property tests use `proptest` to verify invariants with random inputs. They're slower but provide better coverage.

```bash
# Run property tests
cargo nextest run --features prop-tests

# More test cases (default is 10)
PROPTEST_CASES=100 cargo nextest run --features prop-tests
```

## CI

The CI runs all tests with:
```bash
cargo nextest run --workspace --profile ci
```

## Troubleshooting

**Tests timeout**: `cargo nextest run --slow-timeout 120`

**Port conflicts**: Integration tests run sequentially by default. If issues persist: `--test-threads 1`

**Flaky tests**: `cargo nextest run --retries 3`
