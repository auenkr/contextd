#!/usr/bin/env python3
"""Leave-one-out evaluation of speaker links vs adaptive prototypes.

Only labeled embeddings and aggregate metrics are read or printed. No audio,
transcripts, speaker names, or embedding values are written to the report.
"""

import argparse
import json
import math
import sqlite3
from collections import Counter, defaultdict
from pathlib import Path


def normalize(values):
    norm = math.sqrt(sum(value * value for value in values))
    return [value / norm for value in values] if norm > 1e-9 else None


def cosine(left, right):
    if len(left) != len(right):
        return None
    return sum(a * b for a, b in zip(left, right))


def assign(scores, threshold, margin):
    ranked = sorted(scores.items(), key=lambda item: item[1], reverse=True)
    if not ranked or ranked[0][1] < threshold:
        return None
    runner_up = ranked[1][1] if len(ranked) > 1 else -1.0
    if ranked[0][1] - runner_up < margin:
        return None
    return ranked[0][0]


def metrics(rows, threshold, margin, use_prototypes):
    outcomes = []
    skipped = 0
    for index, (truth, query) in enumerate(rows):
        groups = defaultdict(list)
        for other_index, (speaker, vector) in enumerate(rows):
            if other_index != index:
                groups[speaker].append(vector)
        if not groups.get(truth):
            skipped += 1
            continue

        scores = {}
        for speaker, examples in groups.items():
            score = max(cosine(query, example) for example in examples)
            if use_prototypes:
                mean = normalize(
                    [sum(example[d] for example in examples) for d in range(len(query))]
                )
                if mean is not None:
                    prototype_score = cosine(query, mean)
                    if prototype_score is not None:
                        score = max(score, prototype_score)
            scores[speaker] = score
        prediction = assign(scores, threshold, margin)
        outcomes.append((truth, prediction))

    accepted = [(truth, predicted) for truth, predicted in outcomes if predicted is not None]
    correct = sum(truth == predicted for truth, predicted in outcomes)
    false_accepts = sum(truth != predicted for truth, predicted in accepted)
    accepted_correct = len(accepted) - false_accepts
    return {
        "evaluated": len(outcomes),
        "skipped": skipped,
        "correct_of_evaluated": f"{correct}/{len(outcomes)}" if outcomes else "0/0",
        "coverage": len(accepted) / len(outcomes) if outcomes else 0.0,
        "precision_when_assigned": accepted_correct / len(accepted) if accepted else 0.0,
        "false_assignments": false_accepts,
        "left_unknown": len(outcomes) - len(accepted),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "database",
        nargs="?",
        type=Path,
        default=Path.home() / ".local/share/contextd/history.sqlite3",
    )
    parser.add_argument("--threshold", type=float, default=0.75)
    parser.add_argument("--margin", type=float, default=0.04)
    args = parser.parse_args()

    if not args.database.is_file():
        parser.error(f"database not found: {args.database}")
    with sqlite3.connect(f"file:{args.database}?mode=ro", uri=True) as db:
        try:
            stored = db.execute(
                "SELECT speaker_id, embedding FROM speaker_training_examples "
                "WHERE speaker_id IS NOT NULL ORDER BY id"
            ).fetchall()
        except sqlite3.OperationalError as error:
            parser.error(f"speaker learning data is not available: {error}")

    examples = []
    for speaker, serialized in stored:
        vector = normalize(json.loads(serialized))
        if vector is not None:
            examples.append((speaker, vector))

    counts = Counter(speaker for speaker, _ in examples)
    labels = sorted(counts)
    display_counts = ", ".join(
        f"speaker-{index + 1}: {counts[speaker]}" for index, speaker in enumerate(labels)
    ) or "none"
    print(f"Confirmed examples: {len(examples)} across {len(labels)} speakers ({display_counts})")
    print(f"LOO threshold={args.threshold:.2f}, ambiguity margin={args.margin:.2f}")
    if len(labels) < 2 or not any(count >= 2 for count in counts.values()):
        print("Not enough labeled data: need at least two speakers and repeated examples for one speaker.")
        return

    for label, use_prototypes in (("Individual examples", False), ("Examples + prototypes", True)):
        result = metrics(examples, args.threshold, args.margin, use_prototypes)
        print(
            f"{label}: correct={result['correct_of_evaluated']}, "
            f"coverage={result['coverage']:.1%}, "
            f"precision_when_assigned={result['precision_when_assigned']:.1%}, "
            f"false_assignments={result['false_assignments']}, "
            f"left_unknown={result['left_unknown']}, skipped={result['skipped']}"
        )
    print("This is a small leave-one-out check on user-confirmed links, not an independent benchmark.")


if __name__ == "__main__":
    main()
