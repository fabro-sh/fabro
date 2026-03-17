#!/usr/bin/env python3
"""Evaluate SWE-bench predictions using the official harness.

Thin wrapper around swebench.harness.run_evaluation that reads a predictions
JSONL file, runs the evaluation, and prints a summary.
"""

import argparse
import json
from collections import Counter
from pathlib import Path

from swebench.harness.run_evaluation import main as run_evaluation


def print_summary(results_dir: Path):
    """Print evaluation summary from swebench output."""
    results_file = results_dir / "results.json"
    if not results_file.exists():
        print(f"No results file found at {results_file}")
        return

    with open(results_file) as f:
        results = json.load(f)

    total = len(results)
    resolved = sum(1 for r in results.values() if r.get("resolved", False))

    print(f"\n{'=' * 60}")
    print(f"SWE-bench Evaluation Results")
    print(f"{'=' * 60}")
    print(f"Total instances: {total}")
    print(f"Resolved:        {resolved} ({100 * resolved / total:.1f}%)")
    print(f"{'=' * 60}")

    # Per-repo breakdown
    repo_counts: Counter[str] = Counter()
    repo_resolved: Counter[str] = Counter()
    for instance_id, result in results.items():
        repo = instance_id.rsplit("-", 1)[0].rsplit("__", 1)[0]
        # instance_id format: <owner>__<repo>-<number>
        # Extract repo: split on "__" to get owner/repo parts
        parts = instance_id.split("__")
        if len(parts) >= 2:
            repo = f"{parts[0]}/{parts[1].rsplit('-', 1)[0]}"
        else:
            repo = instance_id.rsplit("-", 1)[0]
        repo_counts[repo] += 1
        if result.get("resolved", False):
            repo_resolved[repo] += 1

    print(f"\nPer-repo breakdown:")
    print(f"{'Repo':<40} {'Resolved':>10} {'Total':>8} {'Rate':>8}")
    print(f"{'-' * 40} {'-' * 10} {'-' * 8} {'-' * 8}")
    for repo in sorted(repo_counts):
        res = repo_resolved[repo]
        tot = repo_counts[repo]
        rate = 100 * res / tot if tot > 0 else 0
        print(f"{repo:<40} {res:>10} {tot:>8} {rate:>7.1f}%")


def main():
    parser = argparse.ArgumentParser(
        description="Evaluate SWE-bench predictions"
    )
    parser.add_argument(
        "--predictions",
        type=Path,
        required=True,
        help="Path to predictions JSONL file",
    )
    parser.add_argument(
        "--max-workers",
        type=int,
        default=4,
        help="Number of parallel evaluation workers",
    )
    parser.add_argument(
        "--instance-ids",
        nargs="+",
        help="Evaluate only these instance IDs",
    )
    parser.add_argument(
        "--run-id",
        default="swebench-eval",
        help="Run ID for swebench evaluation output",
    )
    parser.add_argument(
        "--dataset",
        default="princeton-nlp/SWE-bench_Lite",
        help="HuggingFace dataset name",
    )
    parser.add_argument(
        "--split",
        default="test",
        help="Dataset split",
    )
    args = parser.parse_args()

    if not args.predictions.exists():
        print(f"Error: predictions file not found: {args.predictions}")
        raise SystemExit(1)

    print(f"Running swebench evaluation...")
    print(f"  Predictions: {args.predictions}")
    print(f"  Max workers: {args.max_workers}")

    report_dir = str(args.predictions.parent)

    run_evaluation(
        dataset_name=args.dataset,
        split=args.split,
        instance_ids=args.instance_ids or [],
        predictions_path=str(args.predictions),
        max_workers=args.max_workers,
        force_rebuild=False,
        cache_level="env",
        clean=False,
        open_file_limit=4096,
        run_id=args.run_id,
        timeout=1800,
        namespace=None,
        rewrite_reports=False,
        modal=False,
        report_dir=report_dir,
    )

    # Try to find and print results
    results_dir = Path(report_dir) / args.run_id
    if results_dir.exists():
        print_summary(results_dir)
    else:
        print(f"\nEvaluation complete. Check {report_dir}/ directory for results.")


if __name__ == "__main__":
    main()
