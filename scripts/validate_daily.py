#!/usr/bin/env python3
"""Isolated daily-structuring smoke test using real local instruction inference."""
import argparse
import datetime as dt
import json
from pathlib import Path
import sqlite3
import subprocess
import signal
import time
import tempfile
import uuid
from validate_dashboard import Terminal


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=Path('target/debug/contextd'))
    parser.add_argument('--model', type=Path, required=True,
                        help='Cached qwen2.5-1.5b-instruct-q4_k_m.gguf (no copied/downloaded weights)')
    parser.add_argument('--work-dir', type=Path)
    args = parser.parse_args()
    root = args.work_dir or Path(tempfile.mkdtemp(prefix='contextd-daily-validation-'))
    root.mkdir(parents=True, exist_ok=True)
    if (root / 'history.sqlite3').exists():
        raise SystemExit('Use a fresh validation directory')
    binary = args.binary.resolve()
    command = [str(binary), '--data-dir', str(root.resolve())]
    subprocess.run(command + ['paths'], check=True, capture_output=True, text=True)
    model_link = root / 'models/qwen2.5-1.5b-instruct-q4_k_m.gguf'
    model_link.symlink_to(args.model.resolve())
    day = dt.datetime.now().astimezone().date() - dt.timedelta(days=8)
    inputs = [(10, 0, 'We need to release Friday.'),
              (10, 4, 'Payment testing is still pending.'),
              (10, 8, 'Priya will test payments Thursday.'),
              (15, 0, 'The release moved to Monday.')]
    entries = []
    with sqlite3.connect(root / 'history.sqlite3') as connection:
        for hour, minute, text in inputs:
            started_at = dt.datetime.combine(day, dt.time(hour, minute)).astimezone()
            entry = dict(id=str(uuid.uuid4()), created_at=started_at.isoformat(), raw=text,
                         refined=text, speech_model='manual-note', filter_model=None,
                         microphone=None, duration_seconds=0, audio_path=None, filter_error=None)
            entries.append(entry)
            connection.execute('INSERT INTO transcripts VALUES (?, ?, ?)',
                               (entry['id'], entry['created_at'], json.dumps(entry)))
            connection.execute('INSERT INTO memory_fts(id,text) VALUES (?,?)', (entry['id'], text))
    source = root / f'notes/{day}.md'
    source.write_text('\n\n'.join(e['raw'] for e in entries))
    result = subprocess.run(command + ['structure'], check=True, capture_output=True, text=True)
    final = root / f'structured/{day}.md'
    output = final.read_text()
    for entry in entries:
        assert entry['raw'] in output, entry
        assert output.count(entry['id']) == 1, entry
        assert entry['created_at'] in output, entry
    assert not source.exists(), 'Expired transcription was not removed'
    assert 'Monday' in output and 'Friday' in output, 'Correction must not erase earlier plan'
    assert output.count('\n## ') >= 2, 'A later session must be separate from the morning discussion'
    original = final.read_bytes()
    repeat = subprocess.run(command + ['structure'], check=True, capture_output=True, text=True)
    assert not repeat.stdout.strip(), 'Unchanged days should be skipped'
    assert final.read_bytes() == original, 'Restart changed an unchanged final file'
    with sqlite3.connect(root / 'history.sqlite3') as connection:
        assert connection.execute('SELECT count(*) FROM transcripts').fetchone()[0] == len(entries)
    # Remove only isolated generated artifacts to verify automatic restart catch-up.
    final.unlink()
    final.with_suffix('.source.json').unlink()
    source.write_text('\n\n'.join(e['raw'] for e in entries))
    server = subprocess.Popen(command + ['serve', '--port', '0'], stdout=subprocess.DEVNULL,
                              stderr=subprocess.PIPE, text=True)
    try:
        deadline = time.monotonic() + 60
        while not final.exists() and time.monotonic() < deadline:
            if server.poll() is not None:
                raise AssertionError('Server exited before automatic structuring: ' + server.stderr.read())
            time.sleep(0.1)
        assert final.exists(), 'Automatic restart catch-up did not save the final file'
        for entry in entries:
            assert entry['raw'] in final.read_text()
        while source.exists() and time.monotonic() < deadline:
            time.sleep(0.1)
        assert not source.exists(), 'Automatic retention did not expire the transcription file'
    finally:
        server.send_signal(signal.SIGINT)
        _, server_errors = server.communicate(timeout=15)
    terminal = Terminal(str(binary), root, 'explore')
    try:
        terminal.expect('Structured notes')
        terminal.expect('We need to release Friday.')
        terminal.keys('\re')
        terminal.expect('NORMAL EDIT')
        terminal.keys('Edited: ' + '\x13')
        terminal.expect('Saved')
    finally:
        terminal.keys("\x1b")
        terminal.close()
    with sqlite3.connect(root / 'history.sqlite3') as connection:
        edited = json.loads(connection.execute('SELECT document FROM transcripts WHERE id = ?',
                                               (entries[0]['id'],)).fetchone()[0])
    assert edited['raw'] == entries[0]['raw'], 'Browser edit changed the original transcript'
    assert edited['refined'] == 'Edited: ' + entries[0]['refined']
    assert source.exists(), 'Edit should temporarily recreate the original daily file'
    subprocess.run(command + ['structure'], check=True, capture_output=True, text=True)
    assert 'Edited: We need to release Friday.' in final.read_text()
    assert entries[0]['raw'] in final.read_text()
    assert not source.exists(), 'Rebuild should reapply retention'
    report = dict(passed=True, day=str(day), final=str(final), notes=len(entries),
                  transcription_expired=True, originals_indexed=True, restart_skipped=True, automatic_catchup=True, browser_edit_verified=True, server_stderr=server_errors,
                  stderr=result.stderr, restart_stderr=repeat.stderr)
    (root / 'validation-results.json').write_text(json.dumps(report, indent=2))
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    try:
        main()
    except subprocess.CalledProcessError as error:
        raise SystemExit(f"Command failed: {error.cmd}\n{error.stderr}") from error
