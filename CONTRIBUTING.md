# Contributing

## Structure

- `crates/core` (`contextd-core`): capture, segmentation, model downloads, embedded inference, durable queue, SQLite history, Markdown, logging, and loopback retrieval.
- `crates/cli` (`contextd-cli`, binary `contextd`): command parsing, first-run setup, console output, and terminal exploration/audio playback.
- `docs`: installation, configuration, user flows, and model research.
- `scripts`: optional real-model validation and cleanup evaluation.

Keep reusable behavior in the core crate so a future GUI or agent adapter can share it. The product name is defined in `crates/core/src/config.rs` as `PRODUCT_NAME`.

## Build

Follow [installation requirements](docs/INSTALLATION.md), then:

```sh
cargo build --workspace --locked
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
```

Use isolated storage during development:

```sh
cargo run -p contextd-cli -- --data-dir /tmp/contextd-dev start
```

This directory has its own settings and models; it does not reuse the default model cache. Do not commit recordings, transcripts, settings, downloaded weights, or build outputs.

## Validation

When validating a change, run the relevant unit tests and use the existing real-model scripts as needed:

```sh
cargo test -p contextd-core --locked
cargo build -p contextd-cli --locked
python3 scripts/validate_cli.py --work-dir /tmp/contextd-validation --cleanup
```

These scripts download models and generate isolated notes. Add `--microphone` only when testing an actual authorized input device. Full validation requirements and historical results are in [CLI user flows](docs/CLI_USER_FLOWS.md). Historical reports do not certify newer builds or every platform.

For pull requests, describe the user-visible change, commands used for validation, platform tested, and any remaining limitations. Keep implementation, command help, README, and user-flow docs consistent.
