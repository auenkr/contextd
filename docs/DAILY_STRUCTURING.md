# Daily structured notes

The final daily output is `structured/YYYY-MM-DD.md` in the data directory. It groups related speech into named discussions and includes the complete saved text, original transcripts when they differ, individual timestamps, audio sources, and memory IDs. The existing `notes/YYYY-MM-DD.md` transcription file remains temporarily.

For example, notes at 10:00, 10:04, and 10:08 about a release can become one **Release planning** section. A 3:00 PM change from Friday to Monday becomes a separate **Release update** section. Both dates and the original statements remain available. Three-to-five-minute pauses alone do not split a discussion. A pause of 30 minutes starts a new session by default, so a later update stays separate even when it concerns the same topic. Configure `daily_group_gap_minutes` to change this boundary, or use `0` to let the model decide all boundaries.

This is organization, not an abbreviated summary. A local instruction model labels each note with its discussion topic; consecutive matching labels form sections. The application assigns each source note exactly once, in chronological order, and copies its full text. It rejects malformed output and any label count that does not match the notes. An isolated note uses its own first line as its section title. Titles and topic boundaries are model judgments and may need review. Long individual notes use beginning/end excerpts for grouping, while their complete text remains in the output. Small overlapping batches carry discussion context across model limits.

## Automatic processing

With `daily_structure_enabled` on (the default), the recording dashboard and the long-running retrieval server check completed local calendar dates every minute. Processing starts after midnight, rather than 24 hours after app launch. If the app was closed, it catches up on the next dashboard/server launch. Nothing runs while the app is closed or the computer sleeps.

Late queued transcriptions, edited notes, deleted notes, and a changed structuring model cause the affected day to rebuild. Unchanged days are skipped across restarts. An edit makes the browser show updated source notes until the final file is rebuilt. The notes browser otherwise shows the structured file as the final view for that date.

Failed processing preserves transcription files and the previous successful output. It reports an error and retries in one hour; a failed date does not prevent other dates from being processed. Files are replaced atomically and a shared lock prevents competing structuring runs.

The default structuring model is **Qwen 2.5 1.5B Instruct, Q4_K_M** (about 1.1 GB). It downloads when there is a completed day to process, reuses an existing cache, and runs locally. This model is separate from speech recognition and per-utterance cleanup. S1-mini cannot be used for daily organization. Other catalog GGUF instruction models can be selected; custom models and MLX models are not supported for this job yet.

## Transcription file retention

`transcription_retention_days` defaults to **7**. A transcription file is eligible for removal when its local date is at least seven calendar days old, and only after a current structured file has been saved successfully. Retention does not remove an unprocessed or failed day. Set it to **0** to retain transcription files indefinitely. Turning automatic structuring off also stops automatic file expiration.

The final structured Markdown file is retained. This setting deletes only the daily transcription Markdown file: the searchable SQLite index, original text included in the final file, processing logs, and optionally retained audio remain. Search and individual-note editing continue to work. An edit can temporarily recreate an expired transcription file until its structured version is rebuilt.

## Settings and manual processing

These settings are available in **Config** and in `contextd config --json`:

```json
{
  "daily_structure_enabled": true,
  "daily_structure_model": "qwen2.5-1.5b-instruct",
  "daily_group_gap_minutes": 30,
  "transcription_retention_days": 7
}
```

Run the job manually even when automatic processing is disabled:

```sh
# Catch up on all completed dates.
contextd structure

# Rebuild one completed local date.
contextd structure --date 2026-10-10 --force
```

Manual runs also apply the configured transcription-file retention. Today's notes cannot be structured until the day ends. Do not hand-edit generated files: change individual saved notes in the browser and the next run will regenerate their structured day. Copy a final file elsewhere if you want a separately edited document.

To validate real inference, retention, complete-text preservation, restart skipping, automatic catch-up, and browser editing in isolated storage with a cached model:

```sh
cargo build -p contextd-cli --locked
python3 scripts/validate_daily.py \
  --model ~/.cache/contextd/models/qwen2.5-1.5b-instruct-q4_k_m.gguf
```
