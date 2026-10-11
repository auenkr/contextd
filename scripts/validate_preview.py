#!/usr/bin/env python3
"""Hardware-free PTY checks for individual note editing, focus and scrolling."""
import argparse
import json
from pathlib import Path
import sqlite3
import subprocess
import tempfile
from validate_dashboard import Terminal


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', default='target/debug/contextd')
    binary = str(Path(parser.parse_args().binary).resolve())
    with tempfile.TemporaryDirectory(prefix='contextd-preview-') as temporary:
        root = Path(temporary)
        original = '\n'.join(f'Line {i:03d}: Hindi हिंदी and English saved content.' for i in range(100))
        subprocess.run([binary, '--data-dir', str(root), 'note', original], check=True, capture_output=True)
        subprocess.run([binary, '--data-dir', str(root), 'note', 'Second individual note.'], check=True, capture_output=True)
        settings = json.loads(subprocess.check_output([binary, '--data-dir', str(root), 'config', '--json']))
        (root / 'settings.json').write_text(json.dumps(settings))
        terminal = Terminal(binary, root, 'explore')
        terminal.expect('Dates')
        terminal.expect('Line 000')
        # Click a date: SGR mouse coordinates are one-based. It focuses preview.
        terminal.keys('\x1b[<0;6;7M\x1b[<0;6;7m')
        terminal.redraw()
        terminal.expect('Preview focused')
        terminal.keys('jjjjj')
        terminal.output.clear(); terminal.redraw()
        terminal.expect('6 /')
        # Wheel scrolls the content independently of date selection.
        terminal.keys('\x1b[<65;70;15M')
        terminal.output.clear(); terminal.redraw()
        terminal.expect('9 /')
        terminal.keys('g')
        terminal.keys('e')
        terminal.expect('NORMAL EDIT')
        terminal.keys('Edited: ' + '\x13')
        terminal.expect('Saved')
        with sqlite3.connect(root / 'history.sqlite3') as database:
            entries = [json.loads(row[0]) for row in database.execute('SELECT document FROM transcripts ORDER BY created_at')]
        assert entries[0]['raw'] == original
        assert entries[0]['refined'] == 'Edited: ' + original
        assert entries[1]['refined'] == 'Second individual note.'
        # A later recording must not overwrite the edit or disturb selection.
        subprocess.run([binary, '--data-dir', str(root), 'note', 'Another incoming recording.'], check=True, capture_output=True)
        terminal.drain(2.5)
        terminal.keys('n')
        terminal.keys('e')
        terminal.keys('Discard me\x1b')
        terminal.keys('q'); terminal.close()
        settings['editor_mode'] = 'vim'
        (root / 'settings.json').write_text(json.dumps(settings))
        terminal = Terminal(binary, root, 'explore')
        terminal.expect('Dates'); terminal.keys('\re')
        terminal.expect('VIM NORMAL')
        terminal.keys('iVim: ')
        terminal.redraw(); terminal.expect('VIM INSERT')
        terminal.keys('\x1b')
        terminal.keys(':wq\r')
        terminal.expect('Saved')
        terminal.keys('q'); terminal.close()
        found = json.loads(subprocess.check_output([binary, '--data-dir', str(root), 'search', 'Vim']))
        assert found[0]['raw'] == original
        assert found[0]['refined'] == 'Vim: Edited: ' + original
        markdown = next((root / 'notes').glob('*.md')).read_text()
        assert 'Vim: Edited:' in markdown and 'Another incoming recording.' in markdown
    print('PASS: date click focus, arrow/vim scrolling, mouse wheel, Normal/Vim individual edits, save/discard, raw preservation, search and incoming notes')


if __name__ == '__main__':
    main()
