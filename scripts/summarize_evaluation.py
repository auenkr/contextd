#!/usr/bin/env python3
"""Write a shareable Markdown report from the local model comparison evidence."""
import argparse
import json
from pathlib import Path


def cell(value):
    return str(value).replace("|", "\\|").replace("\n", " ").replace("\r", " ")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cleanup", required=True, nargs="+", type=Path)
    parser.add_argument("--speech", required=True, nargs="+", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    models = {}
    for path in args.cleanup:
        for model in json.loads(path.read_text())["models"]:
            models[model["model"]] = model
    cleanup = {"models": list(models.values())}
    speech = [json.loads(path.read_text()) for path in args.speech]
    # A later retry replaces an earlier failed/older run for the same model/mode.
    successful = {run["model"] for report in speech for run in report["runs"] if run.get("cases")}
    runs = {}
    source_errors = []
    for report in speech:
        source_errors.extend(report.get("source_errors", []))
        for run in report["runs"]:
            if run.get("error") and run["model"] in successful:
                continue
            runs[(run["model"], run.get("language"))] = run
    speech = [dict(runs=list(runs.values()), source_errors=source_errors)]
    lines = [
        "# Hindi + English model evaluation", "",
        "Local embedded-runtime comparison, October 2026. These are small diagnostic fixtures,",
        "not full-dataset benchmark scores or a guarantee for every speaker.", "",
        "## Method", "",
        "- Cleanup: 13 English and 10 Hindi/mixed-language fixtures. Strict expected-word matching",
        "  after Unicode NFC normalization, case folding, and punctuation removal. Hindi combining",
        "  marks remain attached; changes in negation, numbers, names, and English spelling count as errors.",
        "- Speech: real recordings from FLEURS Hindi validation, LibriSpeech English validation",
        "  (`hf-internal-testing/librispeech_asr_dummy`), and OpenSLR-104 Hindi-English test",
        "  (`ujs/hinglish`). No synthetic TTS clips. Dataset/config/split/row identify each sample below.",
        "- Word error rate (WER) is total word edits divided by total reference words per suite.",
        "  It can exceed 100% if a model adds many words. Spelling/script differences count as errors.",
        "- Cleanup is disabled during speech scoring; recognition errors and cleanup errors are measured separately.",
        "- Model selections are verified by reloading settings in a fresh CLI process before testing.",
        "  This catches the fixed migration bug that silently replaced multilingual cleanup models with S1-mini.",
        "- Timing is cold CLI wall time including model loading, not warmed-up inference latency.",
        "  Whisper's temperature fallback can produce different results on repeated difficult clips.",
        "- `auto` permits unrestricted language detection. `hi-en` detects Hindi or English and",
        "  retries with a Hindi hint if another language is detected; it never requests translation.",
        "- English-only S1-mini is blocked on Hindi by the new guard. Its mixed cases below",
        "  measure that rejection, not model-generated Hindi cleanup quality.", "",
        "- General cleanup models use quoted transcript data and bilingual editing examples.",
        "  S1-mini retains its upstream training format. Hindi script loss and large expansions",
        "  are rejected; the recording pipeline saves the raw transcript on those failures.", "",
        "## Cleanup results", "",
        "| Model (catalog quantization) | English strict passes | Hindi/mixed strict passes | Cold total seconds |",
        "|---|---:|---:|---:|",
    ]
    for model in cleanup["models"]:
        rows = model.get("cases", [])
        scores = []
        for suite in ["english", "mixed"]:
            selected = [r for r in rows if r["suite"] == suite]
            scores.append(f"{sum(r['passed'] for r in selected)}/{len(selected)}")
        lines.append(f"| {cell(model['model'])} | {scores[0]} | {scores[1]} | {sum(r['seconds'] for r in rows):.1f} |")
        if model.get("error"):
            lines.extend(["", f"Model failure: `{cell(model['model'])}`: {cell(model['error'])}", ""])
    lines += ["", "## Speech results", "",
              "| Model | Language | Suite | Clips | WER | Cold total seconds | Runtime errors |",
              "|---|---|---|---:|---:|---:|---:|"]
    for report in speech:
        for run in report["runs"]:
            for suite, summary in run.get("by_suite", {}).items():
                count = sum(c["suite"] == suite for c in run["cases"])
                lines.append(f"| {run['model']} | {run['language']} | {suite} | {count} | {summary['wer']:.1%} | {summary['seconds']:.1f} | {summary['errors']} |")
            if run.get("error"):
                lines.extend(["", f"Model failure: `{run['model']}`: {cell(run['error'])}", ""])
        for error in report.get("source_errors", []):
            lines.extend(["", f"Source failure ({error['suite']}): {cell(error['error'])}", ""])
    lines += ["", "## Cleanup case outputs", ""]
    for model in cleanup["models"]:
        lines += [f"### {model['model']}", "",
                  "| Case | Input | Actual output or rejected result | Strict match |",
                  "|---|---|---|---|"]
        for row in model.get("cases", []):
            output = row.get("error") or row["output"] or "(empty)"
            lines.append(f"| {cell(row['case'])} | {cell(row['input'])} | {cell(output)} | {'Pass' if row['passed'] else 'Fail'} |")
        lines.append("")
    lines += ["## Speech case outputs", ""]
    for report in speech:
        for run in report["runs"]:
            if not run.get("cases"):
                continue
            lines += [f"### {run['model']} / {run['language']}", "",
                      "| Source and row | Reference | Actual output | WER |",
                      "|---|---|---|---:|"]
            for row in run["cases"]:
                source = f"{row.get('dataset', 'local')} / {row.get('config', '')} / {row.get('split', '')} / {row.get('row', Path(row['file']).name)}"
                lines.append(f"| {cell(source)} | {cell(row['text'])} | {cell(row.get('error') or row['output'])} | {row['wer']:.1%} |")
            lines.append("")
    lines += ["## Reproduce", "", "Build with `cargo build -p contextd-cli`, then:", "",
              "```sh", "python3 scripts/evaluate_cleanup.py --work-dir /tmp/contextd-cleanup \\",
              "  --cache-dir ~/.cache/contextd/models \\",
              "  --models hf.co/superwhisper/s1-mini-GGUF:Q4_K_M qwen3-4b-instruct-2507 \\",
              "    qwen2.5-1.5b-instruct qwen2.5-3b-instruct llama3.2-3b-instruct", "",
              "python3 scripts/evaluate_speech.py --work-dir /tmp/contextd-speech \\",
              "  --cache-dir ~/.cache/contextd/models --languages auto hi-en", "",
              "python3 scripts/summarize_evaluation.py \\",
              "  --cleanup /tmp/contextd-cleanup/cleanup-evaluation.json \\",
              "  --speech /tmp/contextd-speech/speech-evaluation.json \\",
              "  --output model-comparison.md", "```", "",
              "Speech evaluation requires `ffmpeg`. Scripts use isolated app settings and storage,",
              "and may download several gigabytes of weights. Complete cached files can be reused",
              "through `--cache-dir`. Speech supports `--manifest` for your own labeled recordings.",
              "Cleanup returns a nonzero exit code on strict fixture misses; those are recorded",
              "quality results, separate from Rust/TUI test failures.", ""]
    args.output.write_text("\n".join(lines))
    print(args.output)


if __name__ == "__main__":
    main()
