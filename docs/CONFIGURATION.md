# Configuration and storage

The first `contextd start` opens an interactive guide. It selects Parakeet or Whisper Base, offers English cleanup through S1-mini, and asks whether to keep audio. Settings persist; later starts reuse them.

## Edit settings

Stop capture before changing settings:

```sh
contextd config > settings.json
# Edit settings.json, then apply it:
contextd config --file settings.json
contextd start
```

Export the complete configuration before editing: omitted fields receive defaults.

| Field | Default | Meaning |
|---|---|---|
| `microphone` | `null` | Use the default input; otherwise an exact ID from `contextd microphones`. |
| `speech_model` | `parakeet-unified-en-0.6b` | Selected speech model. Use `contextd models` to list available IDs. |
| `filter_enabled` | `false` | Run cleanup after transcription. The first-run guide offers to enable it. |
| `filter_model` | `hf.co/superwhisper/s1-mini-GGUF:Q4_K_M` | The supported English cleanup model. |
| `language` | `auto` | Whisper language detection or a language code such as `en`. Parakeet uses English. |
| `retain_audio` | `false` | Keep completed WAV clips in `audio/`. |
| `speech_threshold` | `0.012` | RMS energy threshold, allowed range `0.001`–`0.2`. Lower it for quieter speech. |
| `silence_ms` | `1400` | Pause that ends a segment, allowed range `300`–`5000` ms. |
| `max_segment_seconds` | `30` | Segment limit during continuous speech, allowed range `5`–`60` seconds. |
| `idle_unload_seconds` | `120` | Release idle models; `0` keeps them loaded, maximum `86400`. |

Model selection and microphone flags persist:

```sh
contextd download whisper-base --select
contextd start --microphone '<exact ID from contextd microphones>'
contextd start --retain-audio
# Stop, then disable retention for future segments:
contextd start --no-retain-audio
```

Disabling retention does not remove previously saved audio. For non-English Whisper transcription, disable S1-mini cleanup; it supports English only.

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
