# logflayer

`logflayer` is a Rust microservice that reads active targets from MongoDB, connects to remote Linux servers over SSH, samples discovered log files, and stores the sampled content back into MongoDB with deterministic deduplication.

## Architecture overview

The service is split into a few focused layers:

- `config`: loads all runtime settings from `.env`.
- `repository`: reads active targets from the source MongoDB collection and writes sampled log records to the destination database.
- `ssh`: handles SSH authentication, remote directory inspection, file discovery, and log sampling.
- `sampling`: defines the sampling abstraction so `first`, `last`, and `both` strategies can be extended later without rewriting SSH code.
- `service`: orchestrates each polling cycle, applies concurrency limits, and guarantees one target failure does not stop the rest.
- `logging`: configures structured JSON logging and size-based file rotation.

## Assumptions about `ai_targets`

The code accepts a practical schema with some defensive alias support. Every active target is expected to provide enough information to reach a Linux host and locate logs. The service currently supports these fields:

```json
{
  "_id": "optional Mongo ObjectId",
  "target_id": "customer-a-prod",
  "status": "active",
  "host": "10.0.0.10",
  "port": 22,
  "username": "ubuntu",
  "password": "optional-password",
  "private_key": "optional inline PEM key",
  "private_key_path": "optional remote or mounted key path",
  "private_key_passphrase": "optional passphrase",
  "log_paths": ["/var/log/app", "/srv/service/logs"],
  "connection": {
    "host": "optional nested alias",
    "port": 22,
    "username": "optional nested alias",
    "log_paths": ["/var/log/app"]
  },
  "credentials": {
    "auth_method": "password or private_key",
    "password": "optional nested alias",
    "private_key": "optional nested alias",
    "private_key_path": "optional nested alias",
    "passphrase": "optional nested alias"
  }
}
```

Validation rules:

- `status` must be `active`.
- `target_id`, `host`, `username`, authentication material, and at least one `log_paths` entry must be present.
- Invalid or malformed target documents are skipped and logged with explicit reasons.
- `target_id` is used as the destination collection name, so invalid MongoDB collection names are rejected.

## Storage model

Source behavior:

- Connects to `SOURCE_DB_NAME`.
- Reads from `SOURCE_COLLECTION_NAME`, which defaults to `ai_targets`.
- Retrieves documents where `status == "active"`.

Destination behavior:

- Writes into `DESTINATION_DB_NAME`, which defaults to `log_samples`.
- Uses one collection per `target_id`.
- Stores at least these fields per sample:
  - `timestamp`
  - `target_id`
  - `source_file`
  - `sample_content`
- Also stores:
  - `host`
  - `path`
  - `sampling_mode`
  - `line_count`
  - `file_size_bytes`
  - `processing_status`
  - `error_details`
  - `sample_hash`

## Deduplication

Each sampled record gets a deterministic SHA-256 `sample_hash`. The hash is built from:

- `target_id`
- `source_file`
- `sampling_mode`
- `sample_content`
- `processing_status`
- `error_details`

That allows the service to skip duplicate inserts when the same file sample is collected repeatedly. A unique MongoDB index is created on `sample_hash` inside every destination collection.

## Log rotation

Structured logs are emitted in JSON format to both stdout and a local file. File rotation is size-based:

- `LOG_MAX_FILE_SIZE_BYTES` sets the maximum size of one log file before rotation.
- `LOG_MAX_FILES` sets how many rotated files are retained.
- Active logs are written to `LOG_DIRECTORY/LOG_FILE_BASE_NAME.log`.
- Rotated files are maintained by the `file-rotate` crate.

## Environment variables

Copy `.env.example` to `.env` and adjust values for your environment.

| Variable | Description |
| --- | --- |
| `MONGODB_URI` | MongoDB connection string. |
| `SOURCE_DB_NAME` | Source database containing target definitions. |
| `SOURCE_COLLECTION_NAME` | Source collection, usually `ai_targets`. |
| `DESTINATION_DB_NAME` | Database that stores sampled logs. |
| `SAMPLE_MODE` | `first`, `last`, or `both`. |
| `SAMPLE_LINE_COUNT` | Number of lines used by the selected sample strategy. |
| `RUN_MODE` | `once` or `periodic`. |
| `POLL_INTERVAL_SECS` | Interval between cycles when `RUN_MODE=periodic`. |
| `CONCURRENCY` | Maximum number of targets processed in parallel. |
| `SSH_TIMEOUT_SECS` | Timeout for TCP connect and SSH read/write operations. |
| `REMOTE_MAX_DEPTH` | Maximum recursive depth for `find` in each log directory. |
| `REMOTE_MAX_FILES_PER_TARGET` | Maximum number of files processed per target directory. |
| `REMOTE_FIND_PATTERNS` | Comma-separated shell patterns such as `*.log,*.out`. |
| `LOG_LEVEL` | `trace`, `debug`, `info`, `warn`, or `error`. |
| `LOG_DIRECTORY` | Directory used for service log files. |
| `LOG_FILE_BASE_NAME` | Base name for rotated service logs. |
| `LOG_MAX_FILE_SIZE_BYTES` | Maximum size of one log file before rollover. |
| `LOG_MAX_FILES` | Number of rotated log files to retain. |

## Error handling

The service handles and logs:

- MongoDB connection failures during startup.
- malformed target documents during source read and validation.
- SSH resolution, network, handshake, and authentication errors.
- missing directories.
- file discovery failures.
- unreadable files.
- empty files.
- duplicate inserts through a unique MongoDB index.

Connection-level SSH failures are recorded as error sample documents using the synthetic `source_file="__target__"` marker so operational failures are visible in the destination database as well as in logs.

## Running locally

1. Create a `.env` file from `.env.example`.
2. Start MongoDB.
3. Build and run:

```bash
cargo run
```

For a periodic worker:

```bash
RUN_MODE=periodic cargo run
```

## Docker

Build:

```bash
docker build -t logflayer .
```

Run:

```bash
docker run --rm --env-file .env logflayer
```

Mount SSH keys or other runtime files when using key-path authentication:

```bash
docker run --rm --env-file .env -v "$PWD/keys:/app/keys:ro" logflayer
```

## Extending the service

Sampling strategies:

- Implement the `Sampler` trait in `src/sampling.rs`.
- Add the new mode to `SamplingMode`.
- Update `build_sampler`.

Authentication methods:

- Extend `AuthMethod` in `src/models.rs`.
- Add the new validation rules in `ValidatedTarget::validate`.
- Implement the SSH login branch in `src/ssh/inspector.rs`.

Remote discovery:

- File discovery currently uses remote `find`.
- If you need SFTP-based traversal, add another discovery implementation in `src/ssh`.

## Crate choices

- `mongodb`: stable async MongoDB driver with index management.
- `tokio`: async runtime and scheduling.
- `ssh2`: practical library that supports both password and key authentication.
- `serde`: target document and sample serialization.
- `dotenvy`: `.env` loading.
- `tracing` and `tracing-subscriber`: structured logging.
- `file-rotate`: size-based log rotation.

## Notes

- The service favors maintainability and explicit logging over schema magic.
- Unit-test scaffolding exists for deterministic hashing and sampling mode parsing.
- The current implementation assumes remote files are plain-text logs readable via standard Linux tools such as `find`, `head`, `tail`, `wc`, and `stat`.
