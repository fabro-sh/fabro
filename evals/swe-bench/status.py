#!/usr/bin/env python3
"""Check status of a running or completed SWE-bench generation or evaluation.

Usage:
    python status.py results/sonnet-baseline           # generation status
    python status.py results/sonnet-baseline/eval      # eval status
"""

import json
import subprocess
import sys
from collections import Counter
from pathlib import Path


def check_gen(d: Path):
    preds = d / "predictions.jsonl"
    results = d / "results.jsonl"

    if not preds.exists() and not results.exists():
        print(f"No results found in {d}")
        return

    pred_rows = [json.loads(l) for l in open(preds)] if preds.exists() else []
    res_rows = [json.loads(l) for l in open(results)] if results.exists() else []

    done = len(pred_rows)
    patched = sum(1 for r in pred_rows if r.get("model_patch", "").strip())
    statuses = Counter(r["status"] for r in res_rows)

    durations = [r["duration_s"] for r in res_rows if r.get("duration_s")]
    avg_dur = round(sum(durations) / len(durations), 0) if durations else 0

    costs = []
    for r in res_rows:
        rd = r.get("fabro_run_dir")
        if rd:
            c = Path(rd) / "conclusion.json"
            if c.exists():
                try:
                    costs.append(json.loads(c.read_text()).get("total_cost", 0))
                except (json.JSONDecodeError, OSError):
                    pass
    total_cost = sum(costs)

    model = "unknown"
    if pred_rows:
        model = pred_rows[0].get("model_name_or_path", "unknown")

    print(f"Generation: {d}")
    print(f"  Model:     {model}")
    print(f"  Progress:  {done}/300")
    print(f"  Patched:   {patched}/{done} ({100 * patched // max(done, 1)}%)")
    print(f"  Avg time:  {avg_dur}s")
    print(f"  Cost:      ${total_cost:.2f} so far")
    print(f"  Statuses:  {dict(statuses)}")


def check_eval(d: Path):
    results_file = d / "eval_results.jsonl"

    if not results_file.exists():
        print(f"No eval results found in {d}")
        return

    rows = [json.loads(l) for l in open(results_file)]
    done = len(rows)
    resolved = sum(1 for r in rows if r.get("resolved"))
    statuses = Counter(r["status"] for r in rows)

    durations = [r["duration_s"] for r in rows if r.get("duration_s")]
    avg_dur = round(sum(durations) / len(durations), 0) if durations else 0

    print(f"Evaluation: {d}")
    print(f"  Progress:  {done}/300")
    print(f"  Resolved:  {resolved}/{done} ({100 * resolved // max(done, 1)}%)")
    print(f"  Avg time:  {avg_dur}s")
    print(f"  Statuses:  {dict(statuses)}")


def check_infra():
    try:
        ps = subprocess.run(
            ["fabro", "ps", "--json"], capture_output=True, text=True, timeout=5
        )
        runs = json.loads(ps.stdout) if ps.stdout.strip() else []
        print(f"  Active:    {len(runs)} fabro runs")
    except Exception:
        print(f"  Active:    (could not reach fabro ps)")

    try:
        sb = subprocess.run(
            ["daytona", "sandbox", "list"], capture_output=True, text=True, timeout=5
        )
        # Count UUIDs in output
        import re
        sandboxes = re.findall(r'[0-9a-f]{8}-[0-9a-f]{4}', sb.stdout)
        count = len(sandboxes) // 3  # each UUID appears ~3 times in the output
        print(f"  Sandboxes: {count} daytona sandboxes")
    except Exception:
        print(f"  Sandboxes: (could not reach daytona)")


def main():
    if len(sys.argv) < 2:
        print("Usage: python status.py <results-dir>")
        print("  e.g. python status.py results/sonnet-baseline")
        print("       python status.py results/sonnet-baseline/eval")
        sys.exit(1)

    d = Path(sys.argv[1])

    # Detect whether this is a gen dir or eval dir
    if (d / "eval_results.jsonl").exists():
        check_eval(d)
    elif (d / "predictions.jsonl").exists() or (d / "results.jsonl").exists():
        check_gen(d)
    else:
        print(f"No results found in {d}")

    print()
    check_infra()


if __name__ == "__main__":
    main()
