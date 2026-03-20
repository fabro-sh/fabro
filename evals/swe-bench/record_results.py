#!/usr/bin/env python3
"""Record SWE-bench eval results into the scoreboard.

Reads generation results + eval results and produces a scoreboard entry
with per-instance data, aggregate stats, and run metadata.

Usage:
    python record_results.py \
        --run-name haiku-baseline-20260316 \
        --gen-dir results/haiku-baseline \
        --eval-dir results/haiku-baseline/eval \
        --description "Haiku 4.5 baseline, default prompt, 10min timeout"
"""

import argparse
import json
import subprocess
from datetime import datetime, timezone
from pathlib import Path

EVAL_DIR = Path(__file__).parent.resolve()
SCOREBOARD_DIR = EVAL_DIR / "scoreboard"


def load_jsonl(path: Path) -> list[dict]:
    rows = []
    with open(path) as f:
        for line in f:
            if line.strip():
                rows.append(json.loads(line))
    return rows


def get_cost_from_fabro_run(fabro_run_dir: str | None) -> float | None:
    """Extract total_cost from a fabro run's conclusion.json."""
    if not fabro_run_dir:
        return None
    conclusion = Path(fabro_run_dir) / "conclusion.json"
    if not conclusion.exists():
        return None
    try:
        data = json.loads(conclusion.read_text())
        return data.get("total_cost")
    except (json.JSONDecodeError, OSError):
        return None


def get_fabro_version() -> str:
    try:
        result = subprocess.run(
            ["fabro", "--version"], capture_output=True, text=True, timeout=5
        )
        return result.stdout.strip()
    except Exception:
        return "unknown"


def main():
    parser = argparse.ArgumentParser(
        description="Record SWE-bench eval results into the scoreboard"
    )
    parser.add_argument(
        "--run-name", required=True,
        help="Name for this run (e.g. haiku-baseline-20260316)",
    )
    parser.add_argument(
        "--gen-dir", type=Path, required=True,
        help="Generation results directory (contains predictions.jsonl, results.jsonl)",
    )
    parser.add_argument(
        "--eval-dir", type=Path, required=True,
        help="Evaluation results directory (contains eval_results.jsonl)",
    )
    parser.add_argument(
        "--description", default="",
        help="Human-readable description of what was tested",
    )
    parser.add_argument(
        "--notes", default="",
        help="Additional notes or observations",
    )
    parser.add_argument(
        "--timeout", type=int, default=1200,
        help="Per-instance timeout used (seconds)",
    )
    parser.add_argument(
        "--sandbox-cpu", type=int, default=2,
        help="CPUs per Daytona sandbox",
    )
    parser.add_argument(
        "--sandbox-memory", type=int, default=4,
        help="Memory (GB) per Daytona sandbox",
    )
    args = parser.parse_args()

    run_dir = SCOREBOARD_DIR / args.run_name
    run_dir.mkdir(parents=True, exist_ok=True)

    # Load generation results
    gen_results = {r["instance_id"]: r for r in load_jsonl(args.gen_dir / "results.jsonl")}
    gen_summary = json.loads((args.gen_dir / "summary.json").read_text())

    # Load eval results
    eval_results = {r["instance_id"]: r for r in load_jsonl(args.eval_dir / "eval_results.jsonl")}
    eval_summary = json.loads((args.eval_dir / "summary.json").read_text())

    # Build per-instance records
    all_instance_ids = sorted(set(gen_results.keys()) | set(eval_results.keys()))
    instances = []
    total_gen_cost = 0.0
    for iid in all_instance_ids:
        gen = gen_results.get(iid, {})
        evl = eval_results.get(iid, {})

        has_patch = bool(gen.get("model_patch", "").strip())
        resolved = evl.get("resolved", False)
        gen_duration = gen.get("duration_s")
        eval_duration = evl.get("duration_s")
        gen_status = gen.get("status", "missing")
        eval_status = evl.get("status", "not_evaluated")
        cost = get_cost_from_fabro_run(gen.get("fabro_run_dir"))
        if cost:
            total_gen_cost += cost

        instances.append({
            "instance_id": iid,
            "has_patch": has_patch,
            "resolved": resolved,
            "gen_status": gen_status,
            "eval_status": eval_status,
            "gen_duration_s": gen_duration,
            "eval_duration_s": eval_duration,
            "gen_cost_usd": round(cost, 6) if cost else None,
            "fabro_run_dir": gen.get("fabro_run_dir"),
        })

    # Write per-instance results
    instances_path = run_dir / "instances.jsonl"
    with open(instances_path, "w") as f:
        for inst in instances:
            f.write(json.dumps(inst) + "\n")

    # Compute aggregates
    total = len(instances)
    patched = sum(1 for i in instances if i["has_patch"])
    resolved = sum(1 for i in instances if i["resolved"])
    resolve_pct = round(100 * resolved / total, 1) if total > 0 else 0
    patch_pct = round(100 * patched / total, 1) if total > 0 else 0

    gen_durations = [i["gen_duration_s"] for i in instances if i["gen_duration_s"] is not None]
    eval_durations = [i["eval_duration_s"] for i in instances if i["eval_duration_s"] is not None]

    # Per-repo breakdown
    from collections import Counter
    repo_total: Counter[str] = Counter()
    repo_resolved: Counter[str] = Counter()
    repo_patched: Counter[str] = Counter()
    for inst in instances:
        parts = inst["instance_id"].split("__")
        repo = f"{parts[0]}/{parts[1].rsplit('-', 1)[0]}" if len(parts) >= 2 else inst["instance_id"]
        repo_total[repo] += 1
        if inst["has_patch"]:
            repo_patched[repo] += 1
        if inst["resolved"]:
            repo_resolved[repo] += 1

    per_repo = {}
    for repo in sorted(repo_total):
        per_repo[repo] = {
            "total": repo_total[repo],
            "patched": repo_patched[repo],
            "resolved": repo_resolved[repo],
            "resolve_pct": round(100 * repo_resolved[repo] / repo_total[repo], 1),
        }

    # Write metadata
    meta = {
        "run_name": args.run_name,
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "model": gen_summary.get("model", "unknown"),
        "provider": gen_summary.get("provider", "unknown"),
        "fabro_version": get_fabro_version(),
        "timeout_s": args.timeout,
        "sandbox_cpu": args.sandbox_cpu,
        "sandbox_memory_gb": args.sandbox_memory,
        "description": args.description,
        "notes": args.notes,
        "total_instances": total,
        "patched": patched,
        "patch_pct": patch_pct,
        "resolved": resolved,
        "resolve_pct": resolve_pct,
        "total_gen_cost_usd": round(total_gen_cost, 2),
        "avg_gen_cost_usd": round(total_gen_cost / total, 4) if total > 0 else 0,
        "gen_wall_time_s": gen_summary.get("total_duration_s"),
        "eval_wall_time_s": eval_summary.get("wall_duration_s"),
        "avg_gen_duration_s": round(sum(gen_durations) / len(gen_durations), 1) if gen_durations else None,
        "avg_eval_duration_s": round(sum(eval_durations) / len(eval_durations), 1) if eval_durations else None,
        "per_repo": per_repo,
    }
    (run_dir / "meta.json").write_text(json.dumps(meta, indent=2) + "\n")

    # Write README
    readme_lines = [
        f"# {args.run_name}",
        "",
        f"**Date:** {meta['timestamp'][:10]}",
        f"**Model:** {meta['model']} ({meta['provider']})",
        f"**Fabro:** {meta['fabro_version']}",
        "",
        f"## Description",
        "",
        args.description or "_No description provided._",
        "",
        f"## Results",
        "",
        f"| Metric | Value |",
        f"|--------|-------|",
        f"| Instances | {total} |",
        f"| Patched | {patched} ({patch_pct}%) |",
        f"| **Resolved** | **{resolved} ({resolve_pct}%)** |",
        f"| Total gen cost | ${meta['total_gen_cost_usd']:.2f} |",
        f"| Avg gen cost | ${meta['avg_gen_cost_usd']:.4f}/instance |",
        f"| Gen wall time | {meta['gen_wall_time_s']}s |",
        f"| Eval wall time | {meta['eval_wall_time_s']}s |",
        "",
        f"## Per-repo breakdown",
        "",
        f"| Repo | Resolved | Total | Rate |",
        f"|------|----------|-------|------|",
    ]
    for repo, stats in per_repo.items():
        readme_lines.append(
            f"| {repo} | {stats['resolved']} | {stats['total']} | {stats['resolve_pct']}% |"
        )

    if args.notes:
        readme_lines.extend(["", "## Notes", "", args.notes])

    (run_dir / "README.md").write_text("\n".join(readme_lines) + "\n")

    # Regenerate leaderboard
    regenerate_leaderboard()

    # Print summary
    print(f"Recorded results for '{args.run_name}'")
    print(f"  Resolved: {resolved}/{total} ({resolve_pct}%)")
    print(f"  Cost: ${meta['total_gen_cost_usd']:.2f} total, ${meta['avg_gen_cost_usd']:.4f}/instance")
    print(f"  Scoreboard: {run_dir}")


def regenerate_leaderboard():
    """Rebuild leaderboard.json from all scoreboard entries."""
    entries = []
    for meta_path in sorted(SCOREBOARD_DIR.glob("*/meta.json")):
        meta = json.loads(meta_path.read_text())
        entries.append({
            "run_name": meta["run_name"],
            "date": meta["timestamp"][:10],
            "model": meta["model"],
            "provider": meta["provider"],
            "resolved": meta["resolved"],
            "total": meta["total_instances"],
            "resolve_pct": meta["resolve_pct"],
            "patched": meta["patched"],
            "patch_pct": meta["patch_pct"],
            "total_cost_usd": meta.get("total_gen_cost_usd"),
            "avg_cost_usd": meta.get("avg_gen_cost_usd"),
        })

    # Sort by resolve rate descending
    entries.sort(key=lambda e: e["resolve_pct"], reverse=True)
    (SCOREBOARD_DIR / "leaderboard.json").write_text(json.dumps(entries, indent=2) + "\n")


if __name__ == "__main__":
    main()
