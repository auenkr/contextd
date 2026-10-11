# Speaker detection and remembered names

Speaker detection is enabled by default. Open **Speakers** from the dashboard or run `contextd speakers`. Select a voice, press Enter to play its sample, and press `n` to name it. Press `l` to link a duplicate or an unknown turn to an existing speaker. Select the target and press Enter. Linked profiles retain all confirmed voice embeddings for future matching; earlier notes and search use the target identity. Unknown turns are corrected individually and do not enroll a voice without an embedding. Names appear in past notes and are reused for future matching recordings.

## Previous and new processing

| Stage | Previous processing | New processing |
| --- | --- | --- |
| Capture | Microphone/device audio, resampled to mono 16 kHz; voice activity detection creates queued clips | Same |
| Transcription | Selected speech model transcribes the entire clip | Same; this complete original transcript is retained |
| Cleanup | Optional local cleanup or translation | Same |
| Speakers | No remembered identity | Local diarization finds speaker turns; an embedding model represents each clear voice as a numeric vector |
| Attribution | One note per clip | A single voice uses the whole transcript; multiple voices trigger additional transcription and optional cleanup of each turn |
| Storage | SQLite/search and daily Markdown; optional full audio | Also stores stable speaker IDs, turn times, voice vectors, and a short sample for each new profile |
| Final output | Daily organization creates the final Markdown | Also includes speaker attribution; renaming refreshes the files and search index |

The diarization worker uses sherpa-onnx, Pyannote segmentation, and a WeSpeaker embedding model. Temporary speaker labels apply only within one clip. Persistent UUIDs connect those labels to saved profiles across recordings. The worker and models run locally; initial setup downloads dependencies and model weights, but recordings are not uploaded.

Each normalized embedding is compared with existing profiles using cosine similarity. The default match threshold is `0.75`, with a minimum lead of `0.04` over the next candidate. These are configurable heuristics, not probabilities or a guarantee of identity. Borderline or competing matches remain **Unknown speaker**. Overlapping speech is not used for enrollment. A new profile requires at least two seconds of clear speech and saves at most fifteen seconds as a mono WAV sample.

Names are metadata keyed by the stable ID. Naming updates profile data, affected transcripts and search, then regenerates Markdown. A cached daily grouping plan allows renaming without another model run. A durable synchronization queue retries failed file updates on the next dashboard/server run; `contextd speakers sync` also retries them. If the grouping plan is no longer valid, the original daily view is updated and the daily worker rebuilds the final file. Literal words in the transcript are preserved, including names actually spoken.

Final files remain in `structured/YYYY-MM-DD.md`. Original transcription Markdown expires after `transcription_retention_days` (default **7**), only after successful daily structuring. Speaker samples and profiles remain available separately, even when `retain_audio` is false. Disabling detection stops new attribution but preserves saved profiles and samples. Existing transcripts are not retroactively analyzed.

## Commands and settings

```sh
contextd speakers
contextd speakers --json
contextd speakers name <speaker-uuid> "Priya"
contextd speakers link <duplicate-uuid> <existing-speaker-uuid>
# Unknown turn indexes are zero based.
contextd speakers unknown
contextd speakers link unknown:<transcript-uuid>:<turn-index> <existing-speaker-uuid>
contextd speakers sync
```

| Setting | Default | Meaning |
| --- | --- | --- |
| `speaker_detection_enabled` | `true` | Enable local detection, matching, and new profile enrollment |
| `speaker_match_threshold` | `0.75` | Minimum cosine similarity; allowed range 0.3–0.95 |
| `speaker_match_margin` | `0.04` | Required lead over the next candidate; allowed range 0–0.3 |
| `speaker_python` | unset | Optional absolute Python executable with sherpa-onnx 1.13.8, NumPy, and certifi installed |

Otherwise `uv` creates a cached Python 3.12 environment in `speaker-runtime/venv`, and models are downloaded into `speaker-runtime/models` under the configured cache directory. Worker diagnostics are in `logs/speaker-worker.log`. Samples are in the data directory's `speakers/` folder; metadata and embeddings are in `history.sqlite3`.

Background noise, short clips, channel changes, and similar voices can affect matching. If speaker processing fails, the complete transcription is still saved and the error is recorded. Several speakers require additional speech-model calls and consequently take longer to process.

Engine reference: [sherpa-onnx speaker diarization](https://k2-fsa.github.io/sherpa/onnx/speaker-diarization/index.html).
