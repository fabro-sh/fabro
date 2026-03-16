#!/usr/bin/env python3
"""SWE-bench evaluation orchestrator for Fabro.

Loads SWE-bench Lite instances, generates per-instance workflow configs,
runs Fabro agent in Daytona sandboxes, and collects patches.

Usage:
    cd evals/swe-bench
    python run_eval.py --output-dir results/haiku-baseline 2>&1 | tee results/haiku-baseline/console.log
"""

import argparse
import json
import logging
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

from datasets import load_dataset
from swebench.harness.constants import MAP_REPO_VERSION_TO_SPECS

from gen_dockerfile import generate_dockerfile, repo_version_key

EVAL_DIR = Path(__file__).parent.resolve()

# ---------------------------------------------------------------------------
# Logging — dual output: file (DEBUG) + terminal (INFO)
# ---------------------------------------------------------------------------

log = logging.getLogger("swe-eval")


def setup_logging(output_dir: Path):
    log.setLevel(logging.DEBUG)
    fmt = logging.Formatter(
        "%(asctime)s  %(levelname)-7s  %(message)s", datefmt="%H:%M:%S"
    )

    # File handler — everything
    fh = logging.FileHandler(output_dir / "eval.log")
    fh.setLevel(logging.DEBUG)
    fh.setFormatter(fmt)
    log.addHandler(fh)

    # Console handler — INFO+
    ch = logging.StreamHandler(sys.stderr)
    ch.setLevel(logging.INFO)
    ch.setFormatter(fmt)
    log.addHandler(ch)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def dot_escape(s: str) -> str:
    """Escape a string for use inside DOT double-quoted attribute values."""
    return s.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")


def load_instances(instance_ids: list[str] | None = None) -> list[dict]:
    """Load SWE-bench Lite instances from HuggingFace."""
    dataset = load_dataset("princeton-nlp/SWE-bench_Lite", split="test")
    instances = [dict(row) for row in dataset]
    if instance_ids:
        id_set = set(instance_ids)
        instances = [i for i in instances if i["instance_id"] in id_set]
        found = {i["instance_id"] for i in instances}
        missing = id_set - found
        if missing:
            log.warning(f"Instance IDs not found: {missing}")
    return instances


def get_spec(instance: dict) -> dict:
    """Get the swebench spec for an instance's (repo, version) pair."""
    repo = instance["repo"]
    version = instance["version"]
    return MAP_REPO_VERSION_TO_SPECS.get(repo, {}).get(version, {})


def build_goal(instance: dict) -> str:
    """Build the goal text from problem statement and hints."""
    parts = [instance["problem_statement"]]
    hints = instance.get("hints_text", "")
    if hints and hints.strip():
        parts.append(f"\n\n## Additional Context\n\n{hints}")
    return "\n".join(parts)


def build_setup_script(instance: dict) -> str:
    """Build the setup script that runs before the agent.

    Clones the repo, checks out the base commit, runs pre_install commands,
    and installs the package. Runs inside the Daytona sandbox.
    """
    spec = get_spec(instance)
    repo = instance["repo"]
    base_commit = instance["base_commit"]
    install_cmd = spec.get("install", "pip install -e .")

    parts = [
        f"git clone https://github.com/{repo}.git .",
        f"git checkout {base_commit}",
    ]

    pre_install = spec.get("pre_install", [])
    if isinstance(pre_install, str):
        pre_install = [pre_install]
    parts.extend(pre_install)

    parts.append(install_cmd)
    return " && ".join(parts)


def toml_literal_string(text: str) -> str:
    """Wrap text in TOML multi-line literal string (no escape processing)."""
    return f"'''\n{text}'''"


def generate_workflow_fabro(instance: dict) -> str:
    """Generate a per-instance .fabro DOT graph with properly escaped values."""
    setup_script = build_setup_script(instance)
    return f'''digraph SWEBench {{
    rankdir=LR
    start [shape=Mdiamond]
    exit  [shape=Msquare]
    setup         [label="Setup", shape=parallelogram, script="{dot_escape(setup_script)}"]
    solve         [label="Solve", prompt="Fix this GitHub issue in the repository. Make the minimal code change needed."]
    extract_patch [label="Extract Patch", shape=parallelogram, script="git diff"]
    start -> setup -> solve -> extract_patch -> exit
}}
'''


def generate_workflow_toml(instance: dict, run_dir: Path) -> str:
    """Generate a workflow.toml config for a single instance."""
    repo = instance["repo"]
    version = instance["version"]
    snapshot_name = repo_version_key(repo, version)
    dockerfile = generate_dockerfile(repo, version)
    fabro_path = run_dir / "workflow.fabro"

    lines = [
        'version = 1',
        f'graph = "{fabro_path}"',
        '',
        '[sandbox]',
        'provider = "daytona"',
        '',
        '[sandbox.env]',
        'PATH = "/opt/miniconda3/envs/testbed/bin:/opt/miniconda3/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"',
        '',
        '[sandbox.daytona.snapshot]',
        f'name = "{snapshot_name}"',
        'cpu = 2',
        'memory = 4',
        'disk = 10',
        f'dockerfile = {toml_literal_string(dockerfile)}',
    ]

    return "\n".join(lines)


def find_patch(run_dir: Path) -> str | None:
    """Find the extract_patch stdout.log in a Fabro run directory."""
    nodes_dir = run_dir / "nodes"
    if not nodes_dir.exists():
        return None

    for node_dir in nodes_dir.iterdir():
        if node_dir.name.startswith("extract_patch"):
            stdout_log = node_dir / "stdout.log"
            if stdout_log.exists():
                return stdout_log.read_text()

    return None


# ---------------------------------------------------------------------------
# Per-instance runner
# ---------------------------------------------------------------------------


def run_instance(
    instance: dict,
    model: str,
    provider: str,
    output_dir: Path,
    timeout: int,
) -> dict:
    """Run Fabro agent on a single SWE-bench instance."""
    instance_id = instance["instance_id"]
    config_dir = output_dir / "configs" / instance_id

    config_dir.mkdir(parents=True, exist_ok=True)

    result = {
        "instance_id": instance_id,
        "model_name_or_path": model,
        "model_patch": "",
        "status": "error",
        "error": None,
        "duration_s": 0,
        "fabro_run_dir": None,
    }

    start_time = time.time()

    try:
        goal_text = build_goal(instance)
        goal_file = config_dir / "goal.txt"
        goal_file.write_text(goal_text)

        fabro_content = generate_workflow_fabro(instance)
        (config_dir / "workflow.fabro").write_text(fabro_content)
        toml_content = generate_workflow_toml(instance, config_dir)
        toml_file = config_dir / "workflow.toml"
        toml_file.write_text(toml_content)

        cmd = [
            "fabro", "run", str(toml_file),
            "--auto-approve",
            "--model", model,
            "--provider", provider,
            "--goal-file", str(goal_file),
            "--no-retro",
            "--label", f"swe-bench={instance_id}",
        ]

        log.debug(f"[{instance_id}] Starting fabro run")
        proc = subprocess.run(
            cmd,
            cwd="/tmp",
            timeout=timeout,
            capture_output=True,
            text=True,
        )

        # Parse the fabro run dir from stderr (format: "    Run:  <path>")
        fabro_run_dir = None
        for line in proc.stderr.splitlines():
            stripped = line.strip()
            if stripped.startswith("Run:") and "/" in stripped:
                fabro_run_dir = Path(stripped.split("Run:", 1)[1].strip().replace("~", str(Path.home())))
                break
        result["fabro_run_dir"] = str(fabro_run_dir) if fabro_run_dir else None

        if proc.returncode != 0:
            result["error"] = f"fabro exited with code {proc.returncode}"
            result["status"] = "failed"
            (config_dir / "fabro_stderr.log").write_text(proc.stderr)
            log.debug(f"[{instance_id}] fabro stderr: {proc.stderr[-300:]}")
        else:
            result["status"] = "completed"

        # Extract patch from the fabro run dir
        if fabro_run_dir:
            patch = find_patch(fabro_run_dir)
        else:
            patch = None
        if patch and patch.strip():
            result["model_patch"] = patch
            result["status"] = "completed"
        elif result["status"] == "completed":
            result["status"] = "no_patch"
            result["error"] = "No patch produced"

    except subprocess.TimeoutExpired:
        result["status"] = "timeout"
        result["error"] = f"Timed out after {timeout}s"
    except Exception as e:
        result["error"] = str(e)
        log.debug(f"[{instance_id}] Exception: {e}")

    result["duration_s"] = round(time.time() - start_time, 1)
    return result


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main():
    parser = argparse.ArgumentParser(
        description="Run SWE-bench evaluation with Fabro"
    )
    parser.add_argument(
        "--model", default="claude-haiku-4-5", help="LLM model to use",
    )
    parser.add_argument(
        "--provider", default="anthropic", help="LLM provider",
    )
    parser.add_argument(
        "--max-workers", type=int, default=20,
        help="Max concurrent sandboxes (default 20)",
    )
    parser.add_argument(
        "--instance-ids", nargs="+", help="Run only these instance IDs",
    )
    parser.add_argument(
        "--timeout", type=int, default=600,
        help="Timeout per instance in seconds",
    )
    parser.add_argument(
        "--output-dir", type=Path,
        default=EVAL_DIR / "results" / "default",
        help="Output directory for results",
    )
    args = parser.parse_args()

    args.output_dir = args.output_dir.resolve()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    setup_logging(args.output_dir)

    log.info("=" * 64)
    log.info("SWE-bench Evaluation")
    log.info("=" * 64)
    log.info(f"  Model:       {args.model}")
    log.info(f"  Provider:    {args.provider}")
    log.info(f"  Workers:     {args.max_workers}")
    log.info(f"  Timeout:     {args.timeout}s")
    log.info(f"  Output:      {args.output_dir}")
    log.info("")

    # --- Load instances ---------------------------------------------------
    log.info("Loading SWE-bench Lite instances...")
    instances = load_instances(args.instance_ids)
    log.info(f"  {len(instances)} instances loaded")
    log.info("")

    # --- Run instances ----------------------------------------------------
    predictions_file = args.output_dir / "predictions.jsonl"
    results_file = args.output_dir / "results.jsonl"

    # Counters (thread-safe via lock)
    lock = threading.Lock()
    counters = {"completed": 0, "no_patch": 0, "failed": 0, "timeout": 0, "error": 0}
    done_count = 0
    total = len(instances)
    wall_start = time.time()

    log.info(f"Running {total} instances (max {args.max_workers} concurrent)...")
    log.info("-" * 64)

    with ThreadPoolExecutor(max_workers=args.max_workers) as executor:
        futures = {
            executor.submit(
                run_instance, inst, args.model, args.provider,
                args.output_dir, args.timeout,
            ): inst
            for inst in instances
        }

        with open(predictions_file, "w") as pf, open(results_file, "w") as rf:
            for future in as_completed(futures):
                result = future.result()
                iid = result["instance_id"]
                status = result["status"]
                dur = result["duration_s"]
                has_patch = bool(result["model_patch"].strip())

                with lock:
                    counters[status] = counters.get(status, 0) + 1
                    done_count += 1
                    n = done_count

                    # Write prediction
                    pf.write(json.dumps({
                        "instance_id": iid,
                        "model_name_or_path": result["model_name_or_path"],
                        "model_patch": result["model_patch"],
                    }) + "\n")
                    pf.flush()

                    # Write detailed result
                    rf.write(json.dumps(result) + "\n")
                    rf.flush()

                # Log every result
                patch_info = f"patch={len(result['model_patch'])}b" if has_patch else "no patch"
                err_info = f"  err={result['error'][:80]}" if result["error"] else ""
                elapsed = round(time.time() - wall_start)
                log.info(
                    f"[{n:3d}/{total}]  {status:<10s}  {dur:6.0f}s  "
                    f"{patch_info:<14s}  {iid}{err_info}"
                )

                # Print running totals every 10 completions
                if n % 10 == 0 or n == total:
                    log.info(
                        f"  --- progress: {n}/{total}  "
                        f"completed={counters.get('completed',0)}  "
                        f"no_patch={counters.get('no_patch',0)}  "
                        f"failed={counters.get('failed',0)}  "
                        f"timeout={counters.get('timeout',0)}  "
                        f"error={counters.get('error',0)}  "
                        f"elapsed={elapsed}s ---"
                    )

    wall_duration = round(time.time() - wall_start, 1)

    # --- Final summary ----------------------------------------------------
    summary = {
        "model": args.model,
        "provider": args.provider,
        "total": total,
        **counters,
        "total_duration_s": wall_duration,
    }
    summary_file = args.output_dir / "summary.json"
    summary_file.write_text(json.dumps(summary, indent=2))

    log.info("")
    log.info("=" * 64)
    log.info("FINAL RESULTS")
    log.info("=" * 64)
    log.info(f"  Total:       {total}")
    log.info(f"  Completed:   {counters.get('completed', 0)}")
    log.info(f"  No patch:    {counters.get('no_patch', 0)}")
    log.info(f"  Failed:      {counters.get('failed', 0)}")
    log.info(f"  Timeout:     {counters.get('timeout', 0)}")
    log.info(f"  Error:       {counters.get('error', 0)}")
    log.info(f"  Wall time:   {wall_duration}s")
    log.info(f"  Predictions: {predictions_file}")
    log.info(f"  Results:     {results_file}")
    log.info(f"  Summary:     {summary_file}")
    log.info(f"  Full log:    {args.output_dir / 'eval.log'}")
    log.info("=" * 64)


if __name__ == "__main__":
    main()
