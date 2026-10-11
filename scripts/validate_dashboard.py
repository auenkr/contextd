#!/usr/bin/env python3
"""Hardware-free PTY smoke checks for setup, dashboard, and cleanup selection."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time


class Terminal:
    """Drive the interactive TUI in a pseudo-terminal, capturing all output."""

    def __init__(self, binary, directory, *args):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 150, 0, 0))
        self.process = subprocess.Popen(
            [binary, "--data-dir", str(directory), *args],
            stdin=slave, stdout=slave, stderr=slave,
        )
        os.close(slave)
        self.output = bytearray()

    def drain(self, seconds=0.3):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if select.select([self.master], [], [], 0.05)[0]:
                try:
                    self.output.extend(os.read(self.master, 65536))
                except OSError:
                    break

    @property
    def screen(self):
        """Terminal text with escape sequences removed, for word-level checks."""
        plain = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", self.output.decode(errors="replace"))
        return re.sub(r"[^\S\n]+", " ", plain)

    def keys(self, text):
        os.write(self.master, text.encode())
        self.drain()

    def expect(self, word, timeout=10):
        """Assert a single word is visible; ratatui splits phrases into cell runs."""
        deadline = time.monotonic() + timeout
        while word not in self.screen and time.monotonic() < deadline:
            self.drain(0.1)
        assert word in self.screen, f"Missing terminal text: {word}"

    def close(self, sig=None):
        if self.process.poll() is None:
            if sig:
                self.process.send_signal(sig)
            else:
                self.keys("q")
        deadline = time.monotonic() + 5
        while self.process.poll() is None and time.monotonic() < deadline:
            self.drain(0.1)
        try:
            assert self.process.wait(timeout=1) == 0
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait()
            os.close(self.master)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/debug/contextd")
    arguments = parser.parse_args()
    binary = str(Path(arguments.binary).resolve())
    with tempfile.TemporaryDirectory(prefix="contextd-tui-") as temporary:
        root = Path(temporary)
        first = root / "first-run"
        terminal = Terminal(binary, first)
        terminal.expect("Welcome")
        terminal.expect("Complete")
        terminal.keys("?")
        terminal.expect("Dashboard")
        terminal.expect("microphones")  # Help content from the shared overlay.
        terminal.keys("\x1b")  # Esc closes help without acting on the setup screen.
        terminal.keys("f")
        terminal.expect("Qwen")
        terminal.keys("j\r")  # Explore notes returns to the unfinished setup.
        terminal.expect("Preview")
        terminal.keys("q")
        terminal.expect("Welcome")
        terminal.keys("\x7f")  # Abort first-run setup; nothing is saved.
        terminal.close()
        assert not (first / "settings.json").exists()

        configured = root / "configured"
        configured.mkdir()
        defaults = json.loads(subprocess.check_output(
            [binary, "--data-dir", str(configured), "config", "--json"], text=True))
        # Unknown speech model exercises automatic startup failure without network/audio.
        (configured / "settings.json").write_text(
            json.dumps({**defaults, "speech_model": "missing-test-model"}))
        terminal = Terminal(binary, configured)
        terminal.expect("Navigation")
        terminal.expect("Unknown")
        terminal.expect("Resume")
        terminal.keys("jjj\r")  # Setup reopens the configuration panel.
        terminal.expect("Welcome")
        terminal.keys("jj\r")  # Back to recording resumes with saved settings.
        terminal.expect("Resume")
        terminal.expect("cancelled")
        terminal.keys("?")
        terminal.expect("Dashboard")
        terminal.keys("q")
        terminal.close(sig=signal.SIGTERM)

        # An externally held capture lock stands in for a second recording process.
        busy = root / "busy"
        (busy / "models").mkdir(parents=True)
        (busy / "models/parakeet-unified-en-0.6b-Q8_0.gguf").touch()
        (busy / "settings.json").write_text(json.dumps(defaults))
        held = open(busy / ".capture.lock", "w")
        fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
        terminal = Terminal(binary, busy)
        terminal.expect("Another")
        terminal.expect("listening")
        terminal.keys("?")
        terminal.expect("Dashboard")
        terminal.keys("q")
        terminal.close()
        held.close()

        # Placeholder cache files exercise selection only, never inference.
        (configured / "settings.json").write_text(json.dumps(defaults))
        models = json.loads(subprocess.check_output(
            [binary, "--data-dir", str(configured), "models"], text=True))
        filters = [model for model in models if model["kind"] == "filter"]
        assert len(filters) == 4
        for filename in ["s1-mini-q4_k_m.gguf", "qwen2.5-1.5b-instruct-q4_k_m.gguf",
                         "qwen2.5-3b-instruct-q4_k_m.gguf", "Llama-3.2-3B-Instruct-Q4_K_M.gguf"]:
            (configured / "models" / filename).touch()
        terminal = Terminal(binary, configured, "config")
        terminal.expect("Speech")
        terminal.keys("\t")
        terminal.expect("Qwen")
        terminal.expect("Llama")
        terminal.keys("?")
        terminal.expect("Configuration")
        terminal.expect("Download")
        terminal.keys("\x1b")
        terminal.expect("cached")
        for index, model in enumerate(filters):
            if index:
                terminal.keys("j")
            terminal.keys("\r")
            terminal.expect("saved")
            settings = json.loads((configured / "settings.json").read_text())
            assert settings["filter_model"] == model["id"]
            assert settings["filter_enabled"]

        # Cache deletion removes one model and keeps the others.
        terminal.keys("\t")  # Speech tab
        for name in ["ggml-tiny.bin", "ggml-base.bin", "parakeet-unified-en-0.6b-Q8_0.gguf"]:
            (configured / "models" / name).touch()
        terminal.keys("j")    # Move off the selected Parakeet entry to Whisper Tiny
        terminal.keys("d")    # Delete a cached model
        terminal.drain(1.5)
        assert not (configured / "models/ggml-tiny.bin").exists(), (
            "cached model was not deleted")
        assert (configured / "models/ggml-base.bin").exists()
        assert (configured / "models/parakeet-unified-en-0.6b-Q8_0.gguf").exists()
        terminal.keys("k")    # Back to the selected Parakeet entry
        terminal.keys("d")    # The model in use cannot be deleted
        terminal.drain(1.0)
        assert (configured / "models/parakeet-unified-en-0.6b-Q8_0.gguf").exists(), (
            "the selected model must stay cached")
        assert json.loads((configured / "settings.json").read_text())["speech_model"] == (
            "parakeet-unified-en-0.6b")
        terminal.close()

        # Downloads started in Config continue on the dashboard after returning.
        background = root / "background"
        background.mkdir()
        (background / "settings.json").write_text(json.dumps(defaults))
        terminal = Terminal(binary, background)
        terminal.expect("Navigation")
        # Raw-mode setup can discard the very first keystrokes; let the UI settle.
        time.sleep(2); terminal.drain(0.5)
        terminal.keys("j\r")       # Config
        terminal.expect("Speech")
        # Queue three speech models with j so two transfer at once and one waits.
        for _ in range(3):
            terminal.keys("j")
            terminal.drain(0.4)
            terminal.keys("\r")
            terminal.drain(1.0)
        terminal.expect("queued")   # The third model is registered but waiting.
        terminal.keys("q")          # Return to the dashboard; downloads survive
        terminal.expect("Downloading")
        terminal.expect("waiting")  # The queued model reports its slot state
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline and not re.search(
                r"Downloading[^|]{0,40}?\d{1,3}%", terminal.screen):
            terminal.drain(1)
        assert re.search(r"Downloading[^|]{0,40}?\d{1,3}%", terminal.screen), (
            "no download progress on the dashboard")
        terminal.close(sig=signal.SIGTERM)

        removed = subprocess.run([binary, "start"], capture_output=True, text=True)
        assert removed.returncode != 0 and "unrecognized subcommand" in removed.stderr
    print("PASS: TUI setup, vim navigation, help overlays, back navigation, Explore return, "
          "concurrent downloads, cache deletion, background progress, SIGTERM, "
          "four cleanup selections, removed start command")


if __name__ == "__main__":
    main()
