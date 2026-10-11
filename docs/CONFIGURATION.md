# Configuration and storage

Run `contextd` to open the recording dashboard. On first run, setup appears inside the TUI: choose speech and cleanup models, audio retention, and microphone, then press Enter. Settings persist; later launches automatically download missing selected models and start recording. The left sidebar contains pause/resume, Config, Explore notes, Setup, and Back; live processing logs appear on the right.

Both screens accept vi-style keys (`j`/`k` to move, `h`/`l` to change model or switch tabs, `g`/`G` for the first and last item) and `?` opens an on-screen help listing every binding. `Backspace` leaves Setup and returns to recording, discarding unsaved changes.

Model downloads report their progress in four places: the bottom gauge with percentage and megabytes, one line per percent in the live log, a **Downloading models** status, and the model screen's own gauge with a queue list. A download never blocks the interface — the sidebar can still be browsed, `?` help still opens, and `Enter` can leave a waiting job to open another screen.

Downloads run in the background: pick several uncached models from the model screen and two transfer at a time while the rest wait for a free slot. Leaving the model screen with `q` does not stop them, and the dashboard keeps reporting their progress until they finish; the microphone starts once the selected models are cached. `Esc`/`Backspace` cancels the transfer and returns to the dashboard, and quitting the dashboard cancels everything. Cancelling removes the partial file, keeps the saved selection, and leaves `Resume recording` ready to retry.

Press `d` in the model screen to delete a cached model and reclaim disk space. The model currently in use is protected, so select a different one first; other cached models are removed together with any partial download.

During setup, `h`/`l` or the left/right arrows choose the speech model, `f` cycles cleanup models, `c` toggles cleanup, `a` toggles audio retention, `m` cycles microphones, and `v` cycles language presets. Choose **Setup** later to revisit these options. Config pauses capture and finishes queued work before editing; returning resumes recording. Explore notes keeps capture running.

## Edit settings

Run `contextd config` in a terminal to open the configuration TUI, styled like
`explore`. Use **1 Speech**, **2 Cleanup**, **3 All settings**, and **4 Microphones**,
or switch sections with Tab/arrows. Press Enter to select a model or edit any config
value. Settings open selectable value lists with descriptions: languages, engines,
On/Off choices, short-transcript policies, and numeric presets. Model fields open
the model browser; either microphone field opens the available-input toggle list.
Each model shows its cache state. Selections save
first, then missing weights download in the background. Selecting a cleanup model enables cleanup.

Press `c` to toggle cleanup, `a` to toggle audio retention, and `q` or Escape to
exit. Leaving Config with `q` keeps downloads running on the dashboard. Settings
changes require capture to be stopped. The screen reloads external file edits every
second while no value is being edited; invalid edits show an error and retain the active settings.
Use arrows or `j`/`k` to browse choices, Enter to apply, and Esc to cancel.
Choose **Custom value…** only for paths, sources, uncommon language codes, or numeric
values outside the presets. `Ctrl+u` clears custom entry; Esc returns to choices.
Empty optional paths/sources clear to `null`. Microphones need no JSON entry:
toggle available inputs with Enter/Space, or choose System default to clear them.

Press `r` to apply device/language suggestions and queue their downloads. Use
`contextd recommend` to inspect the detected CPU, RAM, architecture, and suggestions
without changing settings. These are lightweight choices, not guaranteed accuracy winners.

Use `contextd config --json` to print settings explicitly. Redirected or piped
`contextd config` also prints JSON, keeping existing scripts compatible.

Stop capture before changing settings:

```sh
contextd config --json > settings.json
# Edit settings.json, then apply it:
contextd config --file settings.json
contextd
```

Export the complete configuration before editing: omitted fields receive defaults.

| Field | Default | Meaning |
|---|---|---|
| `microphone` | `null` | Use the default input; otherwise an exact ID from `contextd microphones`. |
| `microphones` | `[]` | Simultaneous input IDs; a nonempty list takes precedence over legacy `microphone`. Each input has its own segmentation state and transcripts include its source. |
| `speech_model` | `parakeet-unified-en-0.6b` | Selected speech model. Use `contextd models` to list available IDs. |
| `filter_enabled` | `false` | Run cleanup after transcription. First-run TUI setup offers to enable it. |
| `filter_model` | `hf.co/superwhisper/s1-mini-GGUF:Q4_K_M` | S1-mini, `qwen3-4b-instruct-2507`, `qwen2.5-1.5b-instruct`, `qwen2.5-3b-instruct`, or `llama3.2-3b-instruct`. |
| `language` | `auto` | Whisper language detection, a language code such as `en` or `hi`, or the `hi-en` mixed-language preset. Parakeet supports only `auto`/`en`. |
| `preferred_language` | `auto` | Recognition hint and model-suggestion input (`en`, `hi`, `hi-en`, or another supported code); explicit `language` overrides it. |
| `cleanup_backend` | `auto` | Infer backend from model ID, or explicitly select `gguf` / `mlx`. MLX requires Apple Silicon macOS. |
| `cleanup_source` | `null` | Custom absolute GGUF file or HTTPS download URL; with MLX, an absolute model folder or Hugging Face repository ID. |
| `mlx_python` | `null` | Optional Python executable with `mlx-lm` installed. Otherwise `uv` creates a cached Python 3.12 environment on first MLX use. |
| `data_directory` | `null` | Absolute notes/audio/database directory override, applied after restart. Existing archives are not moved. |
| `cache_directory` | `null` | Absolute cache override, applied after restart. Existing weights are not moved. |
| `logs_directory` | `null` | Absolute logs override, applied after restart. Config remains at its original location. |
| `short_transcript_policy` | `cleanup` | Discard empty cleanup output; `skip_all` also skips every 1–3-word transcript, `keep` disables this check. |
| `retain_audio` | `false` | Keep completed WAV clips in `audio/`. |
| `speech_threshold` | `0.012` | RMS energy threshold, allowed range `0.001`–`0.2`. Lower it for quieter speech. |
| `vad_threshold` | `0.6` | Embedded Silero speech-confidence threshold (`0.1`–`0.95`), in addition to minimum RMS. Sustained speech onset is required; loud noise alone is insufficient. |
| `silence_ms` | `1400` | Pause that ends a segment, allowed range `300`–`5000` ms. |
| `max_segment_seconds` (legacy) | Ignored | Old configurations may contain this field. Continuous sound now ends only at a pause or when recording stops. |
| `idle_unload_seconds` | `120` | Release idle models; `0` keeps them loaded, maximum `86400`. |

Model selection and microphone flags persist:

```sh
contextd download whisper-base --select
contextd --retain-audio
# Stop, then disable retention for future segments:
contextd --no-retain-audio
```

Choose a microphone with `m` in TUI Setup, or set `microphone` in JSON.
For multiple inputs, open Config → Microphones and toggle each input with Enter/Space.
Selecting System default clears explicit selections. Independent inputs are not mixed,
so overlapping devices can produce duplicate transcripts of the same speech. Live multi-input
validation on the development host is blocked by its macOS microphone permission.

```sh
# Several models download concurrently; --select keeps the last one
contextd download whisper-tiny qwen2.5-1.5b-instruct --select qwen2.5-1.5b-instruct
```
Model IDs come from `contextd models`. Disabling retention does not remove previously saved audio. For non-English Whisper transcription, choose a multilingual cleanup model or disable cleanup; S1-mini supports English only.

### Cleanup model choices

| Model | Download (Q4_K_M) | Purpose |
|---|---:|---|
| S1-mini by Superwhisper | 484 MB | Specialized English transcript normalization; recommended default. |
| Qwen 3 4B Instruct 2507 | 2,497 MB | Non-thinking multilingual cleanup candidate for Hindi + English. |
| Qwen 3.5 0.8B GGUF | 533 MB | Lightweight multilingual cleanup; 9/13 English and 5/10 mixed strict fixtures in the initial run, below 4B. |
| Qwen 3.5 0.8B MLX | ~653 MB | Apple Silicon 4-bit alternative (`qwen3.5:0.8b-mlx`); 8/13 English + 5/10 mixed strict passes; requires Python/MLX runtime. |
| Qwen 2.5 1.5B Instruct | 1,117 MB | Lightweight multilingual, prompt-based cleanup. |
| Qwen 2.5 3B Instruct | 2,105 MB | Larger multilingual, prompt-based cleanup. |
| Llama 3.2 3B Instruct | 2,019 MB | Alternative multilingual instruction model from Meta. |

GGUF models run locally through embedded llama.cpp; MLX uses a persistent local Python worker.
Each model has its own cache file/folder and
generation protocol. The comparison scripts test both English and mixed-language
fixtures; see [Hindi + English evaluation](HINDI_ENGLISH_EVALUATION.md). Superwhisper also offers cloud GPT, Claude, Gemini, and
Llama models; those hosted services are not part of this local catalog.

### Hindi + English

Press `v` in Setup or Config to cycle **auto → English → Hindi → Hindi + English**.
Hindi presets replace English-only Parakeet or lightweight Whisper selections with
Whisper Large-v3, and replace English-only S1-mini with lightweight Qwen3.5 0.8B.
Other selected multilingual models are retained. Setup downloads the selected weights
when saved; in Config, select each missing model with Enter to start its download.

`hi` gives Whisper a Hindi source-language hint. `hi-en` detects the source language
first, keeping English-only utterances working, and retries with a Hindi hint if
Whisper detects a language outside Hindi/English (for example Urdu). Neither mode
requests translation. Larger Whisper models are candidates for higher recognition
accuracy; full `whisper-large-v3` (~3,095 MB) is also available in the catalog.

Additional speech choices are `qwen3-asr-0.6b` (offline, 30 languages including Hindi),
`nemotron-3.5-asr-streaming-0.6b` (multilingual, cache-aware streaming), and
`whisper-small-q8_0` (~264 MB multilingual Whisper Small). Preferred language is a hint,
not a promise of better accuracy: forcing Hindi on English clips can hurt recognition.
Nemotron receives `hi-IN` / `en-US` hints, and automatic mode leaves its prompt unset.
The `hi-en` preset preserves unrestricted Hindi/English detection initially.

Multilingual cleanup is instructed to preserve Devanagari Hindi and English words without translating
or transliterating them. S1-mini rejects Hindi-containing text before loading, and
cleanup that removes all Devanagari text is rejected. During transcription these
failures keep the original text and record `filter_error` rather than discarding it.
Large output expansions (such as answering a dictated question) are also rejected.
General chat models receive bilingual editing examples and quoted transcript data;
S1-mini keeps its specialized upstream format. Supported model selections persist
across restarts instead of being migrated back to S1-mini.
The checks cannot detect every meaning change; raw and cleaned transcripts remain
available together for review.

## Default directories

macOS and Linux share the same layout:

| Purpose | Default | Override base directory |
|---|---|---|
| Settings | `~/.config/contextd/settings.json` | `XDG_CONFIG_HOME` |
| Notes, audio, database, pending jobs | `~/.local/share/contextd/` | `XDG_DATA_HOME` |
| Models | `~/.cache/contextd/models/` | `XDG_CACHE_HOME` |
| Logs and state | `~/.local/state/contextd/` | `XDG_STATE_HOME` |

XDG variables must contain absolute paths; `contextd` is appended to each base. Windows uses platform application directories. Run `contextd paths` to inspect the actual locations.

For a portable directory containing everything:

```sh
contextd --data-dir "$HOME/contextd-data" start
contextd --data-dir "$HOME/contextd-data" explore
contextd --data-dir "$HOME/contextd-data" search "roadmap"
```

Alternatively, set `CONTEXTD_HOME` to that directory. `--data-dir` takes precedence, then `CONTEXTD_HOME`, then the legacy `RECORD_EVERYTHING_HOME`, then platform defaults. Keep the same override in every terminal accessing the same archive. Portable directories are not automatically migrated.

## Console output and files

Normal recording prints a startup message, then completed entries only:

```text
[2026-10-10 21:14:39 +05:30]
  Original text: "Um, review the roadmap tomorrow"
  Saved text: "Review the roadmap tomorrow."
  Note saved: /Users/you/.local/share/contextd/notes/2026-10-10.md
```

Retained audio adds its saved path. Errors remain visible. `--verbose` shows microphone and processing diagnostics. Native model initialization, Metal compilation, and decoder timing messages are suppressed.

`processing.jsonl` contains completion records, including both texts, timestamps, IDs, saved paths, and cleanup failures. `activity.jsonl` retains microphone and processing diagnostics even without `--verbose`. Both append across runs. Console logs go to stderr; JSON command output goes to stdout.

Daily Markdown is generated from SQLite history. Use `contextd note 'your text'` to add notes; hand edits to generated daily files can be overwritten. Temporary queued audio is deleted after successful processing, including when retention is disabled. Completed pending jobs survive interruptions, while an unfinished in-memory segment can be lost after abrupt termination.

## Skip brief filler

By default, `short_transcript_policy` is `cleanup`. Every nonempty transcription reaches the cleanup model when `filter_enabled` is true. If the model returns only whitespace or an empty string, the segment is discarded before any note, database entry, retained audio, or transcript log is saved. There is no word or phrase blacklist. A nonempty result is kept, including useful short phrases.

S1-mini is a transcript normalizer, not a general relevance classifier. If it returns “Okay.”, that result will be saved; empty-output filtering cannot guarantee that every low-context statement is removed. With cleanup disabled, nonempty raw transcripts are kept.

Set the policy to `skip_all` to discard **every** transcript containing one to three words, including useful short statements. Set it to `keep` to preserve even empty cleaned output alongside the original transcript. Older `skip_filler` settings are accepted as an alias for `cleanup`; they no longer enable a phrase list. The rule applies to microphone speech and imported recordings; manually added notes are always kept.

Discarded segments produce no completion summary, searchable entry, or transcript text in logs. Verbose diagnostics record only the segment ID and discard reason. Temporary pending audio is removed when the recording worker finishes the discarded job. Existing saved notes are not removed. Cleanup failures preserve the original text and report the error.
