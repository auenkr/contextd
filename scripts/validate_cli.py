#!/usr/bin/env python3
"""Black-box CLI user-flow validation with isolated data and real local models.
"""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import select
import signal
import socket
import sqlite3
import subprocess
import time
import urllib.error
import urllib.request
import uuid
import wave
import array


class Validation:
    def __init__(self, args):
        self.args = args
        self.root = Path(args.work_dir).resolve()
        self.root.mkdir(parents=True, exist_ok=True)
        self.home = self.root / "data"
        self.env = {**os.environ, "CONTEXTD_HOME": str(self.home)}
        self.binary = str(Path(args.binary).resolve())
        self.results = []
        self.processes = []
        self.defaults = None
        self.sample = self.root / "jfk.wav"
        self.microphone = None

    def run(self, *args, ok=True, timeout=180, env=None):
        result = subprocess.run([self.binary, *args], env=env or self.env,
                                capture_output=True, text=True, timeout=timeout)
        assert (result.returncode == 0) == ok, (
            f"Command {args} returned {result.returncode}: {result.stderr[-4000:]}")
        return result

    def json(self, *args, **kwargs):
        return json.loads(self.run(*args, **kwargs).stdout)

    def config(self, **changes):
        settings = self.json("config")
        settings.update(changes)
        file = self.root / "apply-settings.json"
        file.write_text(json.dumps(settings))
        self.run("config", "--file", str(file))
        return settings

    def check(self, name, function):
        start = time.monotonic()
        first_process = len(self.processes)
        try:
            detail = function() or ""
            result = {"flow": name, "status": "PASS", "detail": detail}
        except Exception as error:
            result = {"flow": name, "status": "FAIL", "detail": str(error)}
            for process in self.processes[first_process:]:
                if process.poll() is None:
                    try:
                        process.send_signal(signal.SIGINT)
                        process.communicate(timeout=15)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.communicate()
        result["seconds"] = round(time.monotonic() - start, 2)
        self.results.append(result)
        print(f"{result['status']:4} {name}: {result['detail']}", flush=True)

    def skip(self, name, reason):
        self.results.append({"flow": name, "status": "SKIP", "detail": reason})
        print(f"SKIP {name}: {reason}", flush=True)

    def start(self, *args, env=None, executable=None):
        process = subprocess.Popen([executable or self.binary, *args], env=env or self.env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.processes.append(process)
        return process

    def wait_listening(self, process):
        deadline = time.monotonic() + 12
        lines = []
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise AssertionError("Recording did not start: " + process.stderr.read())
            ready, _, _ = select.select([process.stderr], [], [], 0.2)
            if ready:
                line = process.stderr.readline()
                lines.append(line)
                if "Listening. Speech is processed automatically." in line:
                    return lines
        raise AssertionError("Microphone startup timed out (permission may be required): " + "".join(lines))

    def stop(self, process, sig=signal.SIGINT, timeout=90):
        process.send_signal(sig)
        output, error = process.communicate(timeout=timeout)
        assert process.returncode == 0, f"Unclean stop ({process.returncode}): {error[-3000:]}"
        return output, error

    def markdown(self, entry):
        date = dt.datetime.fromisoformat(entry["created_at"]).astimezone().date()
        file = self.home / "notes" / f"{date}.md"
        content = file.read_text()
        assert entry["id"] in content and entry["raw"] in content
        assert "## Around " in content
        return file

    def setup(self):
        assert "Commands:" in self.run("--help").stdout
        assert "contextd" in self.run("--version").stdout
        self.defaults = self.json("config")
        assert self.defaults["speech_model"] == "parakeet-unified-en-0.6b"
        assert self.defaults["filter_model"] == "hf.co/superwhisper/s1-mini-GGUF:Q4_K_M"
        assert not self.defaults["filter_enabled"]
        assert self.defaults["microphone"] is None
        assert "--microphone" in self.run("start", "--help").stdout
        return "Fresh setup uses Parakeet, keeps all speech, and exposes the start command"

    def invalid_inputs(self):
        self.run("note", "  ", ok=False)
        self.run("download", "not-a-model", ok=False)
        self.run("delete", "../notes", ok=False)
        self.run("export", "--format", "xml", ok=False)
        self.run("transcribe", str(self.root / "missing.wav"), ok=False)
        bad_wav = self.root / "invalid.wav"
        bad_wav.write_bytes(b"This is not an audio file")
        self.run("transcribe", str(bad_wav), ok=False)
        for change in [{"silence_ms": 1}, {"speech_model": "unknown-speech-model"}]:
            file = self.root / "bad-settings.json"
            file.write_text(json.dumps({**self.defaults, **change}))
            self.run("config", "--file", str(file), ok=False)
        assert self.json("config") == self.defaults
        return "Invalid inputs fail without replacing valid settings"

    def download(self):
        self.reuse_speech("parakeet-unified-en-0.6b-Q8_0.gguf")
        self.run("download", "parakeet-unified-en-0.6b", "--select", timeout=600)
        model = self.home / "models/parakeet-unified-en-0.6b-Q8_0.gguf"
        assert model.stat().st_size > 700_000_000
        assert not list((self.home / "models").glob("*.part"))
        before = model.stat().st_mtime_ns
        self.run("download", "parakeet-unified-en-0.6b", "--select")
        assert model.stat().st_mtime_ns == before
        models = self.json("models")
        assert next(m for m in models if m["id"] == "parakeet-unified-en-0.6b")["installed"]
        with urllib.request.urlopen("https://raw.githubusercontent.com/ggml-org/whisper.cpp/master/samples/jfk.wav") as response:
            self.sample.write_bytes(response.read())
        return "Real default download, atomic install, catalog, and repeat-download behavior"

    def notes(self):
        entry = self.json("note", "Flowcheck: connect personal memory with my project roadmap.")
        file = self.markdown(entry)
        results = self.json("search", "Flowcheck")
        assert any(item["id"] == entry["id"] for item in results)
        assert self.json("search", "nomatch987654") == []
        assert len(self.json("search", "", "--limit", "1")) == 1
        self.json("search", '" OR *')
        assert self.json("export", "--format", "json")
        assert entry["raw"] in self.run("export").stdout
        self.run("delete", entry["id"])
        assert not self.json("search", "Flowcheck")
        assert entry["id"] not in file.read_text()
        return "Add → date/time Markdown → reopen/search → JSON/Markdown export → delete"

    def transcription(self):
        entry = self.json("transcribe", str(self.sample), timeout=180)
        assert "country" in entry["raw"].lower()
        assert entry["raw"] == entry["refined"] and entry["filter_model"] is None
        assert entry["speech_model"] == "parakeet-unified-en-0.6b"
        assert 10 < entry["duration_seconds"] < 12
        self.markdown(entry)
        self.config(retain_audio=True)
        retained = self.json("transcribe", str(self.sample))
        assert Path(retained["audio_path"]).exists()
        self.run("delete", retained["id"])
        assert not Path(retained["audio_path"]).exists()
        self.config(retain_audio=False)
        assert "um, keep these exact words" == self.run("refine", "um, keep these exact words").stdout.strip()
        return "Real Parakeet → full original preserved → daily file; retained audio deletes with its note"

    def switch_speech(self):
        self.reuse_speech("ggml-tiny.bin")
        self.run("download", "whisper-tiny", "--select", timeout=300)
        assert self.json("config")["speech_model"] == "whisper-tiny"
        entry = self.json("transcribe", str(self.sample))
        assert entry["speech_model"] == "whisper-tiny"
        assert "country" in entry["raw"].lower()
        self.markdown(entry)
        self.run("download", "parakeet-unified-en-0.6b", "--select")
        assert self.json("config")["speech_model"] == "parakeet-unified-en-0.6b"
        return "Real Whisper download/inference, persisted default switch, then switch back to Parakeet"

    def microphone_list(self):
        microphones = self.json("microphones")
        assert microphones
        self.microphone = next((m["id"] for m in microphones if "MacBook" in m["name"]), microphones[0]["id"])
        self.config(microphone="disconnected:missing")
        result = self.run("start", ok=False, timeout=15)
        assert "disconnected" in result.stderr
        self.config(microphone=self.microphone, idle_unload_seconds=2)
        assert self.json("config")["microphone"] == self.microphone
        return f"Enumerated actual inputs; selected {self.microphone}; disconnected input fails clearly"

    def recording(self):
        process = self.start("start", "--microphone", self.microphone)
        self.wait_listening(process)
        second = self.run("start", ok=False)
        assert "already listening" in second.stderr
        changed = self.root / "while-recording.json"
        current = self.json("config")
        changed.write_text(json.dumps({**current, "silence_ms": 1800}))
        result = self.run("config", "--file", str(changed), ok=False)
        assert "Stop listening" in result.stderr
        time.sleep(1)
        self.stop(process)
        return "Real microphone starts, duplicate capture/config changes are blocked, Ctrl+C flushes and exits"

    def recovery(self):
        with wave.open(str(self.sample)) as wav:
            assert wav.getnchannels() == 1 and wav.getsampwidth() == 2 and wav.getframerate() == 16000
            samples = [sample / 32768 for sample in array.array("h", wav.readframes(wav.getnframes()))]
        started = dt.datetime.now().astimezone().replace(hour=14, minute=0, second=0, microsecond=0) - dt.timedelta(days=1)
        job_id = str(uuid.uuid4())
        job = {"id": job_id, "settings": self.json("config"), "segment": {"samples": samples, "started_at": started.isoformat()}}
        path = self.home / "pending/recovered-speech.json"
        path.write_text(json.dumps(job))
        process = self.start("start")
        self.wait_listening(process)
        deadline = time.monotonic() + 90
        while path.exists() and time.monotonic() < deadline:
            time.sleep(0.2)
        assert not path.exists(), "Queued speech was not processed"
        time.sleep(7)
        _, stderr = self.stop(process)
        entry = next(e for e in self.json("search", "country", "--limit", "100") if e["id"] == job_id)
        file = self.markdown(entry)
        assert "## Around 2:00 PM" in file.read_text()
        assert file.name == f"{started.date()}.md"
        assert "Models unloaded" in stderr, "Idle-unload event not visible/observed"
        # Replay the same saved job after a crash; its stable ID prevents duplication.
        path.write_text(json.dumps(job))
        process = self.start("start")
        self.wait_listening(process)
        time.sleep(1)
        self.stop(process, sig=signal.SIGTERM)
        assert not path.exists()
        assert sum(e["id"] == job_id for e in self.json("export", "--format", "json")) == 1
        return "Restart backlog → real inference → yesterday 2 PM file; idle unload; replay deduplication; SIGTERM flush"

    def api(self):
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        note = self.json("note", "Agentcontext: roadmap decision to use local Markdown.")
        process = self.start("serve", "--port", str(port), *(["--record"] if self.args.microphone else []))
        deadline = time.monotonic() + 10
        while True:
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{port}/v1/memories?q=Agentcontext&limit=5") as response:
                    data = json.load(response)
                break
            except urllib.error.URLError:
                if time.monotonic() > deadline:
                    raise
                time.sleep(0.1)
        assert any(e["id"] == note["id"] for e in data)
        self.run("serve", "--port", str(port), ok=False)
        self.stop(process)
        return "Agent queries actual loopback API while recording; occupied ports fail; shutdown is clean" if self.args.microphone else "Agent queries actual loopback API; occupied ports fail; shutdown is clean"

    def offline_filter(self):
        self.config(filter_enabled=True)
        self.run("refine", "ship Wednesday", ok=False)
        entry = self.json("transcribe", str(self.sample))
        assert entry["filter_error"] and entry["raw"] == entry["refined"]
        self.markdown(entry)
        self.config(filter_enabled=False)
        self.run("unload")
        return "Missing cleanup model reports a local error; original speech remains in Markdown and search"

    def real_filter(self):
        self.reuse_speech("s1-mini-q4_k_m.gguf")
        self.run("download", self.args.filter_model, "--select", timeout=600)
        assert self.json("config")["filter_model"] == self.args.filter_model
        assert next(m for m in self.json("models") if m["id"] == self.args.filter_model)["installed"]
        self.config(filter_enabled=True, idle_unload_seconds=2)
        text = self.run("refine", "Um, ship this Tuesday, no, Wednesday, with the new design.").stdout.strip()
        assert "wednesday" in text.lower() and "design" in text.lower(), text
        assert "tuesday" not in text.lower() and "um," not in text.lower(), text
        if "s1-mini" in self.args.filter_model:
            assert self.run("refine", "um uh um").stdout.strip() == ""
        entry = self.json("transcribe", str(self.sample))
        assert not entry["filter_error"] and "country" in entry["refined"].lower()
        self.markdown(entry)
        self.run("unload")
        self.config(filter_enabled=False)
        return f"Real {self.args.filter_model} download, selection, embedded cleanup, pipeline, and explicit unloading. Output: {text}"

    def reuse_speech(self, filename):
        if self.args.reuse_from:
            source = Path(self.args.reuse_from) / "data/models" / filename
            destination = self.home / "models" / filename
            if source.exists() and not destination.exists():
                os.link(source, destination)

    def export_scale(self):
        home = self.root / "export-scale"
        env = {**self.env, "CONTEXTD_HOME": str(home)}
        self.run("config", env=env)
        with sqlite3.connect(home / "history.sqlite3") as conn:
            entries = []
            for index in range(10005):
                entry = {"id": str(uuid.uuid4()), "created_at": "2026-10-07T14:00:00+00:00", "raw": f"Archive {index}",
                         "refined": f"Archive {index}", "speech_model": "manual-note", "filter_model": None,
                         "duration_seconds": 0, "audio_path": None, "filter_error": None}
                entries.append((entry["id"], entry["created_at"], json.dumps(entry)))
            conn.executemany("INSERT INTO transcripts VALUES (?, ?, ?)", entries)
        exported = self.json("export", "--format", "json", env=env)
        assert len(exported) == 10005, f"Export truncated: {len(exported)} / 10005 entries"
        return "Long-running archive export includes every note, beyond 10,000 entries"

    def execute(self):
        try:
            for name, function in [
                ("1. First launch and missing-model guidance", self.setup),
                ("2. Invalid arguments/settings", self.invalid_inputs),
                ("3. Download default speech model", self.download),
                ("4. Write, search, export, and delete a note", self.notes),
                ("5. Transcribe real speech and manage audio", self.transcription),
                ("6. Switch speech models", self.switch_speech),
                ("7. Choose microphone", self.microphone_list),
            ]:
                self.check(name, function)
            if self.args.microphone:
                self.check("8. Continuous microphone lifecycle", self.recording)
                self.check("9. Restart recovery and idle unload", self.recovery)
            else:
                self.skip("8–9. Live capture and restart recovery", "Use --microphone to enable actual input capture")
            self.check("10. Retrieve context through local API", self.api)
            self.check("11. Preserve speech when cleanup is offline", self.offline_filter)
            if self.args.cleanup:
                self.check("12. Download and use embedded cleanup model", self.real_filter)
            else:
                self.skip("12. Real cleanup model", "Use --cleanup to download and run S1-mini")
            self.check("13. Export full long-running archive", self.export_scale)
        finally:
            for process in self.processes:
                if process.poll() is None:
                    process.kill()
                    process.communicate()
            report = self.root / "validation-results.json"
            report.write_text(json.dumps(self.results, indent=2))
            print(f"\nEvidence: {report}", flush=True)
        return int(any(result["status"] == "FAIL" for result in self.results))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work-dir", required=True, help="Isolated validation directory; never use personal application data")
    parser.add_argument("--binary", default="target/debug/contextd")
    parser.add_argument("--reuse-from", help="Reuse only downloaded models from a previous isolated run; data/settings remain fresh")
    parser.add_argument("--microphone", action="store_true", help="Briefly open a real microphone to validate lifecycle and restart recovery")
    parser.add_argument("--cleanup", action="store_true", help="Download S1-mini and run the embedded cleanup model")
    parser.add_argument("--filter-model", default="hf.co/superwhisper/s1-mini-GGUF:Q4_K_M", help="Real cleanup model to validate (defaults to the application's recommended model)")
    raise SystemExit(Validation(parser.parse_args()).execute())
