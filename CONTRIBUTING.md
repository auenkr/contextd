# Contributing

## Structure

- `crates/core` (`contextd-core`): capture, segmentation, model downloads, embedded inference, durable queue, SQLite history, Markdown, logging, and loopback retrieval.
- `crates/cli` (`contextd-cli`, binary `contextd`): command parsing, interactive dashboard/TUI setup, terminal exploration/audio playback, model configuration, and the shared background download queue.
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
cargo run -p contextd-cli -- --data-dir /tmp/contextd-dev
```

This directory has its own settings and models; it does not reuse the default model cache. Do not commit recordings, transcripts, settings, downloaded weights, or build outputs.

## Validation

When validating a change, run the relevant unit tests and use the existing real-model scripts as needed:

```sh
cargo test -p contextd-core --locked
cargo build -p contextd-cli --locked
python3 scripts/validate_cli.py --work-dir /tmp/contextd-validation --cleanup
```

Hardware-free terminal regression checks cover configuration and individual note
editing:

```sh
cargo test --workspace --locked
python3 scripts/validate_dashboard.py
python3 scripts/validate_preview.py
```

On macOS, the opt-in device output test plays a generated sentence through the
current output (including headphones), checks its source tag, resampling and VAD,
and optionally transcribes it using a cached speech model:

```sh
CONTEXTD_TEST_MODEL_CACHE="$HOME/.cache/contextd" \
CONTEXTD_TEST_SPEECH_MODEL=qwen3-asr-0.6b \
cargo test --workspace live_device_output_delivers_tagged_speech --locked -- --ignored --nocapture
```

It needs Screen & System Audio Recording access; it does not download models.

These scripts download models and generate isolated notes. Add `--microphone` only when testing an actual authorized input device. Full validation requirements and historical results are in [CLI user flows](docs/CLI_USER_FLOWS.md). Historical reports do not certify newer builds or every platform.

For pull requests, describe the user-visible change, commands used for validation, platform tested, and any remaining limitations. Keep implementation, command help, README, and user-flow docs consistent.
