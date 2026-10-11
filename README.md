# contextd

contextd continuously builds local context from speech for you and your agents. The reusable `contextd-core` Rust crate owns recording, inference, model management, and storage. The `contextd` CLI is an adapter built on that crate.

**Current default:** Parakeet Unified EN 0.6B (Q8_0), full transcription, automatic daily Markdown files. The stored default has cleanup **off**; first-run TUI setup offers to enable it. Original transcripts are preserved either way.

When cleanup is enabled, the default is **"S1-mini" by "Superwhisper"**: a specialized 0.6B transcript-normalization model, with a 462 MiB / 484 MB Q4_K_M download.

## Quick start

Install the build requirements in [Installation](docs/INSTALLATION.md), then:

```sh
git clone https://github.com/auenkr/contextd.git
cd contextd
cargo install --path crates/cli --locked
contextd
```

On first launch, choose your speech model, whether to enable cleanup, and whether to retain audio. Choices persist for later runs. Speak normally and pause: transcription and cleanup run while recording continues. Press Ctrl+C to finish queued work and exit.

```sh
# In another terminal:
contextd explore
contextd search "project roadmap"
contextd paths
```

The command is **contextd**; the GitHub repository is **contextd**. This is an early version: capture runs in the foreground, and no service installer or GUI is included. macOS with Apple Silicon has been exercised locally; Linux and Windows are not yet certified.

### Documentation

- [Install, build, update, and troubleshoot](docs/INSTALLATION.md)
- [Configuration and storage](docs/CONFIGURATION.md)
- [CLI user flows](docs/CLI_USER_FLOWS.md)
- [Cleanup model research and historical evaluation](docs/CLEANUP_MODEL_RESEARCH.md)
- [Contributing and core crate structure](CONTRIBUTING.md)

## What gets saved

macOS and Linux use the same XDG layout:

| Purpose | Default location |
|---|---|
| Settings | `~/.config/contextd/settings.json` |
| Notes, audio, history, pending jobs | `~/.local/share/contextd/` |
| Downloaded models | `~/.cache/contextd/models/` |
| Logs and runtime state | `~/.local/state/contextd/` |

Both respect `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, and `XDG_STATE_HOME` when set to absolute paths. Windows uses its application directories. Run `contextd paths` to see the resolved locations.

The durable data directory contains `notes/`, optional retained WAV clips in `audio/`, recoverable queued jobs in `pending/`, and `history.sqlite3`. Models can be downloaded again if the cache is cleared.

Each daily file looks like:

```markdown
# 2026-10-08

All times are local. Headings indicate approximate speech start times.

## Around 2:00 PM

<!-- memory-id: …; started-at: 2026-10-08T14:00:03+05:30 -->

I want to build a personal memory system that other agents can search.

## Around 2:08 PM

The next step is to connect my project notes.
```

Dates and readable times follow the laptop's local timezone. The precise start timestamp is in an HTML comment for downstream indexing. A segment crossing midnight belongs to its start date. Times are captured before inference, so a slow model doesn't shift a note to the time it finished processing.

Files are updated after transcription and optional cleanup finish. By default, empty cleanup output is discarded before saving; accepted entries preserve both the original and cleaned text. The original appears in a collapsible section when it differs. Cleanup failures save the original and report the error. Completed recording jobs stay in the durable queue until processing and persistence succeed. The Markdown files are generated from the index: put hand edits in separate files or use **Add note**; editing a generated daily file directly will be overwritten when that day is updated.

Bare `contextd` opens a colored recording dashboard and starts capture automatically after setup. The left sidebar offers **Pause / Resume recording**, **Config**, **Explore notes**, **Setup**, and **Quit**. Live microphone activity, original transcripts, cleaned text, saved note paths, and errors appear in the right-hand log panel. Vi keys (`h`/`j`/`k`/`l`, `g`/`G`) navigate, `?` opens help, and `Backspace` leaves Setup and returns to recording.

Missing models download before recording starts. The bottom gauge shows percentage and megabytes, the same progress is echoed line by line into the live log, and the status bar reads **Downloading models**. Downloads run in the background and keep reporting after you leave the screen that started them; selecting two or more models transfers two at a time with the rest queued. Press `Esc` or `Backspace` to abandon a download and stay on the dashboard, `q` to cancel and exit, and `Resume recording` to retry. `d` in the model browser deletes a cached model (the one in use is protected), so large unused models can be rolled back to free disk space. Config pauses recording before editing and resumes it on return; Explore notes keeps recording running.

On first launch, setup appears inside the TUI. Choose speech and cleanup models, microphone, and audio retention, then press Enter to save settings, download missing models with live progress, and begin recording. Clips are off by default; when enabled, processed WAVs are stored under `audio/`. Pending segments are temporary and removed after processing. The dashboard runs continuously in the foreground, with audio processing alongside capture.

Existing installations migrate automatically from the previous macOS `~/Library` locations or single-directory `~/.config/contextd` and `~/.config/record_everything` layouts: settings, models, notes, audio, history, pending jobs, and logs move to their new locations. Capture must be stopped first. Existing destination files are never overwritten. On macOS/Linux, legacy file and directory links keep old paths accessible; stored audio references are updated. Migration is recorded so later searches and exploration can run alongside capture.

`CONTEXTD_HOME` and `--data-dir` select a portable layout with everything under the chosen root: `settings.json`, `models/`, `notes/`, `audio/`, `pending/`, `logs/`, and `history.sqlite3`. Explicit `--data-dir` takes precedence; `RECORD_EVERYTHING_HOME` remains an environment fallback. Portable directories are not automatically migrated. The old `record` subcommand remains an alias for `start`.

To place all app data in a different directory, set:

```sh
export CONTEXTD_HOME="$HOME/contextd"
```

The CLI also accepts `--data-dir <path>` on any command. To change the saved audio preference from the command line, use `--retain-audio` or `--no-retain-audio`; the choice is persisted in `settings.json` and can also be edited through `contextd config`.

## CLI

Building needs stable Rust, a C/C++ compiler, CMake, and Clang (for Rust bindings to the embedded GGUF runtime).

```sh
cargo build -p contextd-cli
# Or install the command in ~/.cargo/bin:
cargo install --path crates/cli --locked

contextd paths
contextd models
contextd download parakeet-unified-en-0.6b --select
contextd microphones
contextd

# Open the recording dashboard (first run includes TUI setup)
contextd
contextd config

# Persist the data location or processed-audio preference
contextd --data-dir "$HOME/contextd" --retain-audio
contextd --no-retain-audio config

# Choose the microphone inside the dashboard's Setup section (m to cycle).

# Transcribe an existing WAV; it also becomes a daily note:
contextd transcribe ./audio.wav

contextd note 'Remember to review the memory index tomorrow.'
contextd search 'memory index' --limit 20
contextd export --format markdown > memory.md
contextd export --format json > memory.json
contextd config
contextd delete <memory-id>

# Browse saved notes and play retained WAV clips:
contextd explore
contextd --explore
contextd --data-dir "$HOME/contextd" explore
```

Ctrl+C (or SIGTERM on macOS/Linux) stops capture, flushes the final utterance, and finishes queued transcriptions. Only one process can own continuous recording in that directory at a time. Changes to settings from another CLI process are blocked until capture stops.

## Processing logs

The dashboard's right-hand panel shows microphone activity, speech detection, queueing, original transcription, final saved text after optional cleanup, and note paths. Native model, Metal compilation, and decoder timing messages are suppressed. Errors and model download progress remain visible. Page Up / Page Down scroll the log; End returns to the latest entries. Activity diagnostics continue to be saved in `activity.jsonl` in the resolved logs directory.

Every saved transcript or manual note produces a completion log on stderr and appends a JSON record to `processing.jsonl` in the resolved logs directory. Records include the completion timestamp, speech/note start time, original text, final saved text, absolute Markdown and history paths, and the retained audio path (or `null` when audio was not retained). Cleanup failures are recorded with the original text that was saved.

```text
[2026-10-09 14:30:00 +05:30]
  Original text: "Um, review the roadmap tomorrow."
  Saved text: "Review the roadmap tomorrow."
  Note saved: /Users/you/.local/share/contextd/notes/2026-10-09.md
```

Logs append across restarts and contain the transcript text. They are kept separately from the note/history files; deleting a note does not erase its earlier log records. To follow them while recording:

```sh
# Resolve the log path on any supported platform:
contextd paths
# macOS default:
tail -f "$HOME/.local/state/contextd/processing.jsonl"
```

Completion logs are written by `contextd-core`, so future interfaces receive the same logging behavior. Console logs use stderr to keep JSON command output on stdout usable by scripts.

Device output audio is available alongside microphones on macOS 13+. Enable
**Config → All settings → Record device audio** (`record_device_audio: true`).
It defaults to **off**. System output is transcribed independently and saved with
its own source label; allow Screen & System Audio Recording for your terminal.

## Explore saved files

`contextd explore` (or `--explore`) opens a terminal file browser with **Notes** and **Audio** tabs. Notes show a styled, scrollable preview grouped by recording time; retained WAV clips play through the default audio output inside the CLI. No external player is needed.

| Key | Action |
|---|---|
| 1 / 2 | Switch Notes / Audio |
| Tab / Shift+Tab | Focus date list / preview |
| Enter / click a date | Focus its preview |
| Up/Down or k/j, mouse wheel | Select dates or scroll focused preview |
| Page Up/Page Down, Home/End, g/G | Scroll the preview |
| n / p | Select the next / previous note |
| e / Enter in notes preview | Edit the individual saved note |
| Ctrl+s | Save edits; preserve the original transcript |
| o | Show / hide originals |
| Enter on audio | Play the selected clip from the beginning |
| Space, s | Pause/resume, stop playback |
| r | Refresh files immediately |
| q, Esc | Return to dates, then exit |
| Ctrl+C | Exit; inside the editor, discard draft |

Choose **Normal** or **Vim** under Config → All settings → Notes editor controls.
Normal uses arrows and direct typing. Vim supports `h/j/k/l`, `w/b`, `gg/G`,
`i/a/A/o`, Esc, `:w` and `:wq`. Edits update search and daily Markdown while
preserving the original transcript and surviving later recordings.

The browser refreshes every two seconds and uses the same data directory as recording, including `--data-dir` or `CONTEXTD_HOME`. Run it in a second terminal while continuous recording stays active. Explore does not start capture, download models, or run the setup guide. Audio appears only when retention was enabled while recording (`--retain-audio`); discarded clips cannot be played. File listing and note access are exposed by `contextd_core::explore` for reuse by future interfaces; terminal rendering and playback controls belong to the CLI.

## Switchable models

### Speech to text

| Model | Approx. download | Notes |
|---|---:|---|
| **Parakeet Unified EN 0.6B Q8_0** | 731 MB | Default; English; `transcribe.cpp` GGUF backend |
| Whisper Tiny | 75 MB | Multilingual; smallest / fastest Whisper |
| Whisper Base | 142 MB | Multilingual; balanced |
| Whisper Small | 466 MB | Multilingual; higher accuracy |
| Whisper Small Q8_0 | ~264 MB | Lower-memory multilingual Small |
| Qwen3 ASR 0.6B | ~850 MB | Offline multilingual ASR; Hindi + English |
| Nemotron 3.5 Streaming 0.6B | ~751 MB | Multilingual ASR with cache-aware streaming |
| Whisper Large v3 Turbo | 1.6 GB | Multilingual; larger memory footprint |
| Whisper Large v3 | 3.1 GB | Full multilingual model; accuracy-oriented alternative |

Download and select a speech model with `contextd download <model-id> --select`. Switching replaces the previous loaded model. Apple Silicon builds enable Metal; other platforms use CPU by default. Parakeet Unified EN is English-only; select Whisper for other languages.

For **Hindi + English**, press `v` in Setup or Config until the language is `hi-en`.
The preset upgrades lightweight/English-only speech selections to Whisper Large-v3 and
replaces S1-mini with lightweight multilingual Qwen3.5 0.8B. When Nemotron is selected,
the mixed preset switches to Qwen3 ASR with a Hindi hint to preserve Devanagari and
English script together. Whisper keeps automatic detection with a Hindi retry.
See [configuration](docs/CONFIGURATION.md#hindi--english) and the
[real-model comparison](docs/HINDI_ENGLISH_EVALUATION.md) for outputs and measured errors.

### Optional cleanup LLM

The CLI downloads the selected cleanup GGUF into the app's `models/` directory and runs it inside the application using the embedded `llama.cpp` runtime. No model server is required.

Default: **"S1-mini" by "Superwhisper"**, downloaded as `s1-mini-q4_k_m.gguf`. This model is fine-tuned specifically for speech-transcript normalization: fillers, repeated words, abandoned starts, explicit self-corrections, punctuation, and spoken numbers. It is English-only.

Config → **All settings → Saved text language** controls translation after recognition.
The default is **Same as spoken** (`output_language: "same"`). Choose **English**
(`"en"`), Hindi, French, or another supported target to clean up and translate saved
notes while retaining the original transcript. Selecting a target enables cleanup.
Cleanup models are classified as **supports translation** or **does not support translation**;
the class appears in Config and in `contextd models` JSON as `translation_support`.
S1-mini does not translate: selecting it shows an explicit warning and restricts
saved text to Same as spoken. Qwen and Llama cleanup models support translation,
with language coverage and accuracy depending on the selected model. Custom
models have unclassified capabilities; choose only languages they support.

```sh
contextd download hf.co/superwhisper/s1-mini-GGUF:Q4_K_M --select
```

This selects S1-mini for cleanup. Unsupported legacy Ollama names are migrated to
S1-mini; supported multilingual catalog selections survive restarts. Qwen3 4B Instruct
2507, Qwen 2.5 1.5B/3B, and Llama 3.2 3B are also available locally.
Qwen3.5 0.8B is available as GGUF (`qwen3.5:0.8b`) and Apple Silicon MLX
(`qwen3.5:0.8b-mlx`). The lightweight model scored below 4B on cleanup fixtures.
See [lightweight model results](docs/LIGHTWEIGHT_MODEL_EVALUATION.md).

Config now exposes every setting, including preferred language, backend/source,
storage paths, retention, and simultaneous microphone selections. It reloads external
settings-file edits while idle. Use `contextd recommend` for device/language suggestions;
storage changes apply after restart without moving existing files.

The core uses S1-mini's documented training format, greedy decoding, and required empty thinking prefix through embedded llama.cpp. It uses `semi-formal` / `prose` / `general`. S1-mini needs this specialized format; it is not sent generic chat instructions or few-shot examples. Its style can also normalize spoken numbers and colloquial phrasing.

Enable cleanup in TUI Setup, or export settings with `contextd config --json > settings.json`, set `filter_enabled` to `true`, and apply them with `contextd config --file settings.json` while capture is stopped. The prompt removes fillers, repetition, abandoned starts, and explicit self-corrections with minimal word changes. It preserves names, numbers, substantive detail, tone, and language. For example:

```text
Before: Um, ship this Tuesday, no, Wednesday, with the new design.
After:  Ship this Wednesday with the new design.
```

Model quality varies; accepted entries preserve the original transcript. S1-mini passed 11/13 strict local cleanup cases in an earlier runtime evaluation; this is an application fixture, not a general accuracy benchmark. It can still collapse intentional emphasis. If model loading fails or output is truncated, the raw transcript remains saved and the error is shown. S1-mini's empty output for pure filler is valid; these segments are discarded by default. See [model research and evaluation](docs/CLEANUP_MODEL_RESEARCH.md) for sources, cases, and limitations.

Model downloads need internet access; speech and cleanup inference stay on the laptop.

### Auto-unload

The `idle_unload_seconds` setting controls both engines:

- Default: **120 seconds**. The core unloads speech and cleanup models after the idle timeout.
- **0**: keep loaded.
- `contextd unload`: release them immediately; they reload for the next utterance.

## Recording behavior

- Multiple selected microphones, independent mono 16 kHz resampling, Silero speech-aware segmentation.
- Default pause boundary: 1.4 seconds. Continuous sound stays in one segment until a pause or recording stops; there is no timed cutoff.
- Recognition runs after that boundary. For audio longer than 30 seconds, streaming-capable models use one streaming recognition session with internal audio windows; these do not split the saved entry or trigger cleanup. Other models use their whole-buffer recognition path. Active audio stays in memory until the pause, then enters the durable processing queue.
- Cleanup receives the complete transcript when it fits its 4,096-token context (including prompt and output). Longer transcripts are split near sentence/paragraph boundaries and cleaned parts are saved together as one entry.
- A 300 ms preroll helps retain the beginning of speech; very brief noise is discarded.
- Completed segments are queued on disk while inference catches up. Failed jobs remain for retry on the next recording start. Temporary pending audio is removed after successful processing even when **Keep audio clips** is off.
- Empty output from the cleanup model is discarded before saving, with no word or phrase blacklist. Set `short_transcript_policy` to `skip_all` to discard every 1–3-word transcript, or `keep` to disable filtering; see [configuration](docs/CONFIGURATION.md#skip-brief-filler).
- Raw WAV clips can optionally be retained alongside text. Deleting a memory removes its retained clip and updates the daily file.

This captures microphone audio, including voices/background sounds reaching the selected inputs. It does not capture system audio, screens, or identify speakers. Speech confidence and minimum energy may need tuning for a noisy room or quiet voice. Synthetic fan-noise tests pass; the user's actual fan and simultaneous live inputs still need microphone-permitted testing. Suspend/sleep pauses the OS's audio capture; the app cannot record while the laptop sleeps. Abrupt termination can lose the currently unfinished segment; completed queued segments are recoverable.

On macOS, capture checks microphone authorization and requests access before opening the input stream. For an embedded terminal, allow its host app (such as T3 Code) under **System Settings → Privacy & Security → Microphone**. If access was denied, enable it there and restart the host app. Activity logs show RMS in scientific notation so quiet input is distinguishable from exact zeros, and report missing buffers or zero-valued input explicitly.

Some terminal host builds lack the macOS `com.apple.security.device.audio-input` entitlement. macOS blocks microphone access for their child processes without showing a permission dialog; the CLI cannot grant that entitlement to its host. Use Ghostty or Terminal for capture in that case. The TUI explorer can still run in the embedded terminal.

## Personal context for agents

The immediate output is ordinary dated Markdown, ready for Obsidian or another agent's RAG index. A read-only full-text retrieval interface is also available:

```sh
contextd serve --port 3210
# Or run capture and retrieval together:
contextd serve --port 3210 --record

curl 'http://127.0.0.1:3210/v1/memories?q=project+decision&limit=20'
```

It is disabled by default and accessible only on loopback. Responses include memory IDs, start times, full original and optionally refined text, model IDs, and durations. Search uses SQLite FTS5/BM25 lexical ranking; it is not embedding/vector search. Local agents can cite the memory ID and timestamp or index `notes/*.md` themselves. No MCP server is included in this initial version.

## Architecture

```text
crates/core/                    Shared recording + model + note pipeline
  src/audio.rs                  Microphone enumeration/capture, resampling, segmentation
  src/inference.rs              Embedded Parakeet/Whisper + llama.cpp cleanup
  src/models.rs                 Downloadable catalog and default selections
  src/queue.rs                  Disk-backed speech backlog
  src/history.rs                SQLite index + atomic daily Markdown generation
  src/context.rs                Loopback retrieval API
crates/cli/                     CLI adapter
```

## Verification

```sh
cargo test -p contextd-core
cargo clippy --workspace --all-targets -- -D warnings

# Downloads the default model and tests real inference with a public JFK WAV:
CONTEXTD_HOME=/tmp/contextd-smoke \
  cargo run -p contextd-core --example smoke
```

The smoke check verifies Parakeet transcription, daily Markdown output, search, and unloading. It needs approximately 731 MB of free space and network access on its first run.

For black-box user-flow validation, including real downloads, Parakeet/Whisper inference, restart recovery, optional in-process S1-mini cleanup, and an archive exceeding 10,000 notes:

```sh
cargo build -p contextd-cli
python3 scripts/validate_cli.py \
  --work-dir /tmp/contextd-validation \
  --microphone --cleanup
```

Use a fresh validation directory. `--microphone` briefly opens an actual input device; omit it to skip live capture/recovery checks. `--cleanup` downloads S1-mini and runs real local cleanup; omit it to skip that flow. The script requires Python 3, internet for first-time downloads, and roughly 1.4 GB of model storage. It writes `validation-results.json` and uses isolated test data. See [CLI user flows](docs/CLI_USER_FLOWS.md) for commands, checkpoints, and validation scope.

## Local data and privacy

Inference runs locally. Model downloads contact the upstream hosting services; downloaded weights have their own licenses. Completed notes and JSON logs contain the original and processed transcript text. Audio retention is off by default, but completed segments temporarily enter the disk-backed processing queue. Deleting a note does not erase earlier log records.

The optional retrieval API binds to loopback without authentication. Enable it only when local applications should be able to query your notes. No telemetry or hosted transcription service is configured.

## License

Application source is licensed under [MIT](LICENSE). Dependencies and downloaded models retain their respective licenses. Model weights are downloaded separately and are not included in this repository.

## References

- [llama.cpp](https://github.com/ggml-org/llama.cpp): embedded GGUF cleanup inference.
- [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp): embedded Parakeet Unified / Whisper runtime.
- [whisper.cpp](https://github.com/ggml-org/whisper.cpp): Whisper models and local inference design.
- ["S1-mini" by "Superwhisper"](https://huggingface.co/superwhisper/s1-mini): the specialized default cleanup model. Model attribution and license links are in [research notes](docs/CLEANUP_MODEL_RESEARCH.md).

The desktop GUI is intentionally not included in the current workspace. A future GUI can depend on `contextd-core` as a crate, just as the CLI does.
