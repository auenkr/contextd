# CLI user flows and validation

The main everyday flow is **pick storage → download Parakeet → pick microphone → record → read that day's Markdown**. Search, export, cleanup, and agent retrieval are optional.

## 1. First-time setup

```sh
cargo install --path crates/cli --locked
export CONTEXTD_HOME="$HOME/contextd"
contextd --help
contextd config
contextd models
contextd download parakeet-unified-en-0.6b --select
```

**Checkpoint:** Parakeet Unified EN 0.6B is default; stored settings default to cleanup off, while first-run TUI setup offers cleanup enabled. The downloaded model appears as installed. Repeat downloads leave its file intact. `contextd` opens TUI setup if settings have not yet been saved, downloads missing selected models, and begins recording. Keep the same storage environment variable in later sessions; otherwise platform directories are used (inspect them with `contextd paths`). Existing single-directory installations are migrated automatically.

## 2. Select microphone and record

```sh
contextd microphones
contextd
```

Choose **Setup** and press `m` to cycle available microphones. Selection persists, so next time:

```sh
contextd
```

**Checkpoint:** the dashboard shows **Listening**, navigation on the left, and live processing logs on the right. Missing models download first with the gauge, per-percent log lines, and **Downloading models** status; several selected models transfer at once and progress continues after switching screens. `Esc` cancels the transfer and returns to the dashboard. Arrow keys and `h`/`j`/`k`/`l` move and change selections, `g`/`G` jump to the first and last item, and `?` opens a help panel listing every binding. `Backspace` leaves Setup without saving; `q`, Esc, or Ctrl+C stops recording and exits. Speak and pause; entries appear in the log and `notes/YYYY-MM-DD.md`. Another process cannot record into the same directory or change settings during capture. Config pauses recording before editing; returning resumes it.

Each saved entry shows original and saved text and the absolute note path in the dashboard log. Speech start time, completion time, IDs, retained audio paths, and history paths are retained in `processing.jsonl` in the logs directory shown by `contextd paths` (or `<data-dir>/logs` for portable storage). Manual notes and imported recordings also generate completion logs; cleanup failures identify the original text that was saved.

Dashboard activity logs show the selected microphone, speech detection, queueing, transcription, and cleanup. Every ten seconds an input summary reports how much audio arrived, its peak RMS signal level, the speech threshold, and whether speech is being collected. These lines are saved in `activity.jsonl` in that logs directory. If no completion appears, this distinguishes missing audio input from input that stays below the speech threshold or a segment still being processed.

## 3. Stop and read notes

Press **Ctrl+C**. SIGTERM also finishes gracefully on macOS/Linux.

**Checkpoint:** the last utterance is flushed, queued jobs finish, and the process exits successfully. Daily Markdown looks like:

```markdown
# 2026-10-08

## Around 2:00 PM

I want to connect these notes to my personal memory system.
```

Date/time headings refer to approximate speech **start**, not inference completion. Full transcription is saved by default.

### Browse while recording continues

In a second terminal, use the same data directory:

```sh
contextd explore
# Equivalent flag:
contextd --explore
# For a custom recording directory:
contextd --data-dir "$HOME/contextd" explore
```

**Checkpoint:** the terminal shows Notes and Audio tabs and refreshes every two seconds. Use `1/2` to switch folders. Arrows select dates; Enter or a click focuses the preview, where arrows, `j/k`, the wheel and Page Up/Page Down scroll. Tab switches focus. `n/p` selects a note, `e` edits its saved text, and Ctrl+s saves without changing its original transcript. Config → Notes editor controls selects Normal or Vim. Enter on an audio file starts playback, Space pauses/resumes, and `s` stops it. `q`/Esc returns to the date list, then exits; Ctrl+C exits directly and restores the terminal. Recording in the other terminal continues. Empty folders show an explanation; audio is available only when it was retained. Explore works without speech models or setup and requires an interactive terminal.

### Include device output

Open Config → All settings → Record device audio and choose On, then return to
recording. It defaults to Off. On macOS 13+, allow Screen & System Audio Recording
for the terminal if requested. The dashboard lists both microphone and device
output inputs; notes from output audio show their source. Cleanup and saved-language
translation also apply to device audio. Disable the setting to resume microphone-only
capture. A missing permission produces an actionable error.

## 4. Add and retrieve thoughts

```sh
contextd note 'Project roadmap: connect daily notes to my agent index.'
contextd search 'project roadmap' --limit 20
contextd search '' --limit 20
```

**Checkpoint:** manual notes share the daily files. Search survives process restarts and returns IDs, timestamps, text, and model provenance. It is lexical full-text retrieval, not semantic/vector search.

## 5. Import a recording

```sh
contextd transcribe ./recording.wav
```

**Checkpoint:** JSON contains real local-model output, also saved to Markdown/search. Invalid WAVs fail clearly. For imported audio without a supplied recording timestamp, approximate start is current time minus clip duration, not the original file's historical recording time.

## 6. Switch speech models

Stop recording, then:

```sh
contextd download whisper-tiny --select
contextd transcribe ./recording.wav
contextd download parakeet-unified-en-0.6b --select
```

**Checkpoint:** selection persists and the transcript names the model actually used. Parakeet Unified EN is English-only; select Whisper for other languages.

## 7. Tune recording and offloading

```sh
contextd config --json > settings.json
# Edit the file:
contextd config --file settings.json
```

Useful fields: `microphone`, `silence_ms`, `speech_threshold`, `retain_audio`, `idle_unload_seconds`. Continuous sound is no longer split on a timer.

**Checkpoint:** settings persist. Invalid values and unknown speech models fail. Idle models unload (reported with `--verbose`); new speech reloads them. `0` keeps them loaded. Missing JSON fields use defaults, so begin with a complete exported config when preserving existing choices matters.

## 8. Optional local cleanup

```sh
contextd download hf.co/superwhisper/s1-mini-GGUF:Q4_K_M --select
contextd config --json > settings.json
# Set filter_enabled to true, then:
contextd config --file settings.json
contextd refine 'Um, ship this Tuesday, no, Wednesday, with the new design.'
contextd
```

**Validated S1-mini output:** `Ship this Wednesday with the new design.`

**Checkpoint:** original speech is saved before local cleanup. If the model is missing or cannot load, the original remains saved and an error is recorded. When wording changes, the original remains in Markdown too.

The default is **"S1-mini" by "Superwhisper"**, a 0.6B model fine-tuned for this task, with English-only support. It runs in-process through the embedded GGUF runtime and uses its specialized non-thinking generation protocol. Pure-filler empty output is accepted. See [cleanup research](CLEANUP_MODEL_RESEARCH.md) for the broader comparison and limitations.

To translate saved notes, choose a multilingual cleanup model, then open
**Config → All settings → Saved text language** and select **English** or another
supported target. **Same as spoken** is the default. Selecting a translation target
enables cleanup; originals remain available. S1-mini is explicitly classified as
not supporting translation, displays a warning, and offers only Same as spoken.
Qwen and Llama are classified as supporting translation; their available languages
and translation accuracy vary. See [saved text language](CONFIGURATION.md#saved-text-language-and-translation).

## 9. Resume after interruption

```sh
contextd
```

**Checkpoint:** completed `pending/` jobs are retried with original times/settings, then removed on success. Replay uses a stable ID and does not duplicate a persisted note. Failed jobs remain queued and are reported. An unfinished in-memory segment cannot survive SIGKILL; Ctrl+C/SIGTERM flushes it.

## 10. Export or delete

```sh
contextd export --format markdown > all-notes.md
contextd export --format json > all-notes.json
contextd delete <memory-id>
```

**Checkpoint:** exports include all notes, beyond 10,000 entries. Delete updates search and Markdown and removes retained audio if present.

## 11. Agent retrieval while recording

```sh
contextd serve --port 3210 --record
# Another terminal/tool:
curl 'http://127.0.0.1:3210/v1/memories?q=project+roadmap&limit=20'
```

**Checkpoint:** recording continues while the loopback API returns memories with IDs/timestamps. Omit `--record` for retrieval only. Occupied ports fail. Ctrl+C shuts down and flushes recording.

## Automated validation scope

`scripts/validate_cli.py` invokes the actual executable with fresh, isolated storage and real Parakeet, Whisper Tiny, and optional S1-mini. It writes a JSON result report. Its 13 scenarios cover setup, invalid input, model installation, notes/search/export/delete, transcription/audio retention, switching, microphone selection, capture/locks, backlog/idle-unload/SIGTERM, concurrent API, missing-model fallback, real in-process S1-mini cleanup/unloading, and export of 10,005 notes.

Physical microphone opening/stopping is tested. Deterministic transcription/restart checks use a public JFK WAV and persisted audio jobs; they do not prove live word-for-word accuracy in every acoustic environment. Silence segmentation/date rollover also have Rust unit coverage. Validation is on macOS/Apple Silicon, not Windows/Linux certification or a multi-day soak test.

## Results — October 8, 2026

The 13-pass report below is historical validation from before the move to in-process llama.cpp inference. Re-run the script after building the current CLI to validate this implementation; its old real-cleanup result does not cover the new runtime.

| Scenario | Result |
|---|---|
| First launch/defaults/missing-model guidance | PASS |
| Invalid arguments, settings, and WAVs | PASS |
| Default model install/catalog/repeated download | PASS |
| Notes/daily Markdown/search/export/delete | PASS |
| Real Parakeet transcription and retained audio deletion | PASS |
| Real Whisper transcription and persisted model switching | PASS |
| Microphone enumeration/selection/disconnected-input error | PASS |
| Microphone lifecycle/duplicate recording lock/config lock/Ctrl+C | PASS |
| Backlog replay/previous-day 2 PM heading/idle unload/deduplication/SIGTERM | PASS |
| Agent API while recording/port conflict/graceful shutdown | PASS |
| Missing cleanup model fallback retaining original text | PASS |
| Real S1-mini cleanup, pure-filler handling, and explicit/automatic LLM unloading | PASS |
| Complete export of 10,005 notes | PASS |

Validation found and fixed cross-process configuration changes during capture, truncated exports at 10,000 entries, missing graceful SIGTERM handling, and missing CLI idle-unload visibility. The CLI's version banner now uses its command name. Cleanup received explicit self-correction examples; Qwen 3B passed, while the smaller 1.5B model still failed the correction-quality test.

Additional checks passed: eight Rust unit tests (including S1-mini's exact protocol and empty-output handling), strict workspace Clippy (all targets), and TypeScript/Vite production build. The repeatable runner writes its full raw results to `<validation directory>/validation-results.json`.

## Organize a completed day

1. Keep the dashboard running across midnight, or launch it later to catch up.
2. Open **Explore notes** and select the date: the final structured view groups related entries and preserves every original detail.
3. Change **Config → Transcription file retention** to choose how long the original daily Markdown stays (default: 7 days; 0: forever).
4. For a manual catch-up, run `contextd structure`; use `--date YYYY-MM-DD --force` to rebuild a completed date.

Only successfully structured transcription files expire. Search and individual-note edits still use the retained index. [Details and validation](DAILY_STRUCTURING.md).

## Name a speaker

Open **Speakers** from the dashboard, select an unnamed voice, press Enter to play the sample, and press `n` to enter a name. Enter saves; Esc cancels. Past notes update and future confident matches reuse this name. Press `s` to stop playback and `q` to return. `contextd speakers --json` lists profiles; `contextd speakers name <id> "Name"` names one without opening the screen. Run `contextd speakers train` to immediately retrain the local matching weights from confirmed links.
