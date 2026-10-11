#!/usr/bin/env python3
"""Compare embedded Whisper models on real Hindi, English and mixed test audio.

Requires ffmpeg on PATH. Downloads only selected clips via the HF datasets API.
Use --manifest to evaluate your own [{file, text, suite}] recordings instead.
"""
import argparse
import json
from pathlib import Path
import subprocess
import time
import urllib.parse
import urllib.request
import urllib.error

from evaluate_cleanup import apply_settings, call, prepare_storage, words

SOURCES = [
    ("hindi", "google/fleurs", "hi_in", "validation", "transcription"),
    ("english", "hf-internal-testing/librispeech_asr_dummy", "clean", "validation", "text"),
    ("mixed", "ujs/hinglish", "default", "test", "sentence"),
]


def edit_distance(reference, hypothesis):
    previous = list(range(len(hypothesis) + 1))
    for i, left in enumerate(reference, 1):
        current = [i]
        for j, right in enumerate(hypothesis, 1):
            current.append(min(current[-1] + 1, previous[j] + 1,
                               previous[j - 1] + (left != right)))
        previous = current
    return previous[-1]


def benchmark_clips(directory, count, suites):
    clips = directory / "clips"
    clips.mkdir(exist_ok=True)
    cases, errors = [], []
    for suite, dataset, config, split, text_field in SOURCES:
        if suite not in suites:
            continue
        query = urllib.parse.urlencode(dict(dataset=dataset, config=config, split=split,
                                            offset=0, length=count))
        try:
            try:
                with urllib.request.urlopen("https://datasets-server.huggingface.co/rows?" + query, timeout=120) as response:
                    rows = json.load(response)["rows"]
            except urllib.error.HTTPError:
                # The row-serving and cached-preview endpoints fail independently.
                preview = urllib.parse.urlencode(dict(dataset=dataset, config=config, split=split))
                with urllib.request.urlopen("https://datasets-server.huggingface.co/first-rows?" + preview, timeout=120) as response:
                    rows = json.load(response)["rows"][:count]
            for item in rows:
                row, index = item["row"], item["row_idx"]
                audio = row["audio"]
                url = audio[0]["src"] if isinstance(audio, list) else audio["src"]
                original = clips / f"{suite}-{index}.audio"
                wav = clips / f"{suite}-{index}.wav"
                if not wav.exists():
                    urllib.request.urlretrieve(url, original)
                    subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(original),
                                    "-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le", str(wav)], check=True)
                cases.append(dict(file=str(wav), text=row[text_field], suite=suite,
                                  dataset=dataset, config=config, split=split, row=index))
        except Exception as error:
            errors.append(dict(suite=suite, error=str(error)))
            print(f"Source failed: {suite}: {error}", flush=True)
    (directory / "manifest.json").write_text(json.dumps(cases, indent=2, ensure_ascii=False))
    return cases, errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work-dir", required=True)
    parser.add_argument("--binary", default="target/debug/contextd")
    parser.add_argument("--cache-dir")
    parser.add_argument("--models", nargs="+", default=["whisper-small", "whisper-large-v3-turbo", "whisper-large-v3"])
    parser.add_argument("--languages", nargs="+", default=["auto", "hi", "hi-en"])
    parser.add_argument("--clips-per-suite", type=int, default=3)
    parser.add_argument("--suites", nargs="+", choices=["hindi", "english", "mixed"], default=["hindi", "english", "mixed"])
    parser.add_argument("--manifest", type=Path, nargs="+", help="One or more saved/local clip manifests")
    args = parser.parse_args()
    if args.clips_per_suite < 1:
        parser.error("--clips-per-suite must be positive")
    directory = Path(args.work_dir).resolve()
    env = prepare_storage(directory, args.cache_dir)
    binary = str(Path(args.binary).resolve())
    if args.manifest:
        cases = []
        for manifest in args.manifest:
            selected = json.loads(manifest.read_text())
            for case in selected:
                case["file"] = str((manifest.resolve().parent / case["file"]).resolve())
            cases.extend(selected)
        errors = []
    else:
        cases, errors = benchmark_clips(directory, args.clips_per_suite, args.suites)
    report = dict(runtime="embedded transcribe.cpp; cold CLI invocation per clip",
                  source_errors=errors, runs=[])
    evidence = directory / "speech-evaluation.json"
    evidence.write_text(json.dumps(report, indent=2, ensure_ascii=False))
    if not cases:
        return 1
    for model in args.models:
        print(f"Preparing speech model: {model}", flush=True)
        try:
            call(binary, env, "download", model, timeout=3900)
        except subprocess.SubprocessError as error:
            message = getattr(error, "stderr", None) or str(error)
            report["runs"].append(dict(model=model, error=message))
            print(f"Model failed: {model}: {message}", flush=True)
            evidence.write_text(json.dumps(report, indent=2, ensure_ascii=False))
            continue
        for language in args.languages:
            apply_settings(binary, env, directory, speech_model=model, language=language,
                           filter_enabled=False, short_transcript_policy="keep")
            run = dict(model=model, language=language, cases=[])
            report["runs"].append(run)
            for case in cases:
                start = time.monotonic()
                error, output = None, ""
                try:
                    raw = call(binary, env, "transcribe", case["file"], timeout=600).stdout.strip()
                    output = json.loads(raw)["raw"] if raw else ""
                except subprocess.CalledProcessError as failure:
                    error = failure.stderr.strip()
                except (subprocess.TimeoutExpired, ValueError) as failure:
                    error = str(failure)
                reference = words(case["text"])
                edits = edit_distance(reference, words(output))
                row = {**case, "output": output, "error": error, "edits": edits,
                       "reference_words": len(reference), "wer": round(edits / max(1, len(reference)), 4),
                       "seconds": round(time.monotonic() - start, 3)}
                run["cases"].append(row)
                print(f"{model}/{language} {case['suite']} WER={row['wer']}: {error or output}", flush=True)
                evidence.write_text(json.dumps(report, indent=2, ensure_ascii=False))
            run["by_suite"] = {}
            for suite in sorted({case["suite"] for case in cases}):
                rows = [r for r in run["cases"] if r["suite"] == suite]
                run["by_suite"][suite] = dict(
                    wer=round(sum(r["edits"] for r in rows) / max(1, sum(r["reference_words"] for r in rows)), 4),
                    seconds=round(sum(r["seconds"] for r in rows), 3),
                    errors=sum(r["error"] is not None for r in rows))
            evidence.write_text(json.dumps(report, indent=2, ensure_ascii=False))
    print(f"Evidence: {evidence}")
    return int(bool(errors) or any("error" in r or any(c["error"] for c in r["cases"]) for r in report["runs"]))


if __name__ == "__main__":
    raise SystemExit(main())
