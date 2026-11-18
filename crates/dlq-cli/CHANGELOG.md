# Changelog

All notable changes to the `dlq` CLI are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.2.0]

### Added
- `dlq redrive <source> <destination> [-n LIMIT]` moves messages, e.g. from a DLQ back
  to its source queue. If anything isn't moved, it explains what was rejected (with
  SQS error codes), which FIFO groups stopped, and which messages were delivered but
  not deleted (so a re-run would duplicate them).
- `dlq purge <url>` deletes every message in a queue. Asks you to type the queue name;
  pass `--yes` in scripts.
- `dlq completions <shell>` and `dlq man` generate shell completions and a man page.
- `dlq --version`.
- `RUST_LOG` controls logging to stderr (default: warnings and errors).
- Prebuilt binaries for Linux, macOS, and Windows on GitHub releases.
- Credentials from `aws login` are supported.

### Changed
- Exits with status 1 on errors (previously 0, or 101 on panics), and prints the full
  error cause chain.
- Network and AWS errors in `list` and `poll` are reported instead of panicking.
- `poll` prints each batch as it arrives.
- Closing stdout early (e.g. `dlq poll ... | head`) exits quietly instead of panicking.
- `job load` stores non-JSON lines as JSON strings instead of rejecting them.
- `send-batch` claims pending items with one query and syncs SQLite less often (WAL).
- Minimum supported Rust version is 1.94.1.

### Security
- Dropped the AWS SDK's legacy hyper 0.14 / rustls 0.21 client, which had unpatched
  advisories, and replaced `chrono` (unmaintained) with `time`.

## [1.1.0]

Previous published release.
