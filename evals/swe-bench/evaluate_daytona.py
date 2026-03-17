#!/usr/bin/env python3
"""Evaluate SWE-bench predictions using Daytona sandboxes.

Reuses the same Daytona snapshots from the generation phase. For each
prediction, creates a sandbox, applies the model patch + test patch,
runs the test suite, and grades the result using swebench's log parsers.

Usage:
    cd evals/swe-bench
    python evaluate_daytona.py \
        --predictions results/haiku-baseline/predictions.jsonl \
        --output-dir results/haiku-baseline/eval \
        2>&1 | tee results/haiku-baseline/eval/console.log
"""

import argparse
import base64
import json
import logging
import re
import subprocess
import sys
import tempfile
import threading
import time
from collections import Counter
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

from datasets import load_dataset
from swebench.harness.constants import MAP_REPO_VERSION_TO_SPECS
from swebench.harness.grading import (
    get_eval_tests_report,
    get_resolution_status,
    get_logs_eval,
)
from swebench.harness.test_spec.test_spec import make_test_spec

from gen_dockerfile import generate_dockerfile, repo_version_key

EVAL_DIR = Path(__file__).parent.resolve()


def load_completed_ids(output_dir: Path) -> set[str]:
    """Load instance IDs that have already been evaluated from prior runs."""
    completed = set()
    results_file = output_dir / "eval_results.jsonl"
    if results_file.exists():
        with open(results_file) as f:
            for line in f:
                if line.strip():
                    try:
                        completed.add(json.loads(line)["instance_id"])
                    except (json.JSONDecodeError, KeyError):
                        pass
    return completed

log = logging.getLogger("swe-eval-grade")

HEREDOC_DELIMITER = "EOF_114329324912"
START_TEST_OUTPUT = ">>>>> Start Test Output"
END_TEST_OUTPUT = ">>>>> End Test Output"
APPLY_PATCH_FAIL = ">>>>> Patch Apply Failed"

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------


def setup_logging(output_dir: Path):
    log.setLevel(logging.DEBUG)
    fmt = logging.Formatter(
        "%(asctime)s  %(levelname)-7s  %(message)s", datefmt="%H:%M:%S"
    )
    fh = logging.FileHandler(output_dir / "eval_grade.log")
    fh.setLevel(logging.DEBUG)
    fh.setFormatter(fmt)
    log.addHandler(fh)

    ch = logging.StreamHandler(sys.stderr)
    ch.setLevel(logging.INFO)
    ch.setFormatter(fmt)
    log.addHandler(ch)


# ---------------------------------------------------------------------------
# Build eval script
# ---------------------------------------------------------------------------


def get_test_directives(instance: dict) -> list[str]:
    """Extract test file directives from test_patch."""
    diff_pat = r"diff --git a/.* b/(.*)"
    directives = re.findall(diff_pat, instance["test_patch"])
    non_test_exts = [".txt", ".md", ".rst", ".csv", ".json", ".xml", ".yml", ".yaml"]
    directives = [
        d for d in directives if not any(d.endswith(ext) for ext in non_test_exts)
    ]
    if instance["repo"] == "django/django":
        transformed = []
        for d in directives:
            d = d[: -len(".py")] if d.endswith(".py") else d
            d = d[len("tests/"):] if d.startswith("tests/") else d
            d = d.replace("/", ".")
            transformed.append(d)
        directives = transformed
    return directives


def get_modified_files(patch: str) -> list[str]:
    """Extract modified file paths from a unified diff."""
    return re.findall(r"diff --git a/.* b/(.*)", patch)


def build_eval_script(instance: dict, model_patch: str) -> str:
    """Build a shell script that applies patches and runs tests.

    Returns a bash script string that:
    1. Clones the repo and checks out the base commit
    2. Installs the package
    3. Applies the model patch
    4. Resets test files, applies the test patch
    5. Runs the test command with output markers
    """
    repo = instance["repo"]
    version = instance["version"]
    base_commit = instance["base_commit"]
    test_patch = instance["test_patch"]
    spec = MAP_REPO_VERSION_TO_SPECS.get(repo, {}).get(version, {})

    install_cmd = spec.get("install", "pip install -e .")
    test_cmd_base = spec.get("test_cmd", "pytest -rA")
    if isinstance(test_cmd_base, list):
        test_cmd_base = test_cmd_base[-1]
    test_directives = get_test_directives(instance)
    test_cmd = " ".join([test_cmd_base] + test_directives)

    test_files = get_modified_files(test_patch)
    reset_tests = f"git checkout {base_commit} {' '.join(test_files)}"
    apply_test_patch = (
        f"git apply -v - <<'{HEREDOC_DELIMITER}'\n{test_patch}\n{HEREDOC_DELIMITER}"
    )

    pre_install = spec.get("pre_install", [])
    if isinstance(pre_install, str):
        pre_install = [pre_install]

    eval_commands = spec.get("eval_commands", [])
    if isinstance(eval_commands, str):
        eval_commands = [eval_commands]

    lines = [
        "#!/bin/bash",
        "set -e",
        "",
        "# Clone and setup",
        f"git clone https://github.com/{repo}.git .",
        f"git checkout {base_commit}",
    ]

    for cmd in pre_install:
        lines.append(cmd)

    lines.append(install_cmd)

    # Eval environment setup (locale, etc.)
    for cmd in eval_commands:
        lines.append(cmd)

    lines.extend([
        "",
        f"git config --global --add safe.directory /home/daytona/workspace",
        "",
        "# Apply model patch (non-fatal — record failure in output)",
        f"if ! git apply -v - <<'{HEREDOC_DELIMITER}'",
        model_patch,
        HEREDOC_DELIMITER,
        "then",
        f"  echo '{APPLY_PATCH_FAIL}'",
        "  exit 1",
        "fi",
        "",
        "# Re-install after patching (some repos need this)",
        install_cmd,
        "",
        "# Stop aborting on error — test failures are expected",
        "set +e",
        "",
        "# Reset test files and apply test patch",
        reset_tests,
        apply_test_patch,
        "",
        "# Run tests",
        f"echo '{START_TEST_OUTPUT}'",
        test_cmd,
        f"echo '{END_TEST_OUTPUT}'",
        "",
        "# Clean up test files",
        reset_tests,
    ])

    return "\n".join(lines)


def toml_literal_string(text: str) -> str:
    return f"'''\n{text}'''"


def generate_eval_toml(instance: dict, config_dir: Path) -> str:
    """Generate a workflow.toml for running the eval script."""
    repo = instance["repo"]
    version = instance["version"]
    snapshot_name = repo_version_key(repo, version)
    dockerfile = generate_dockerfile(repo, version)

    lines = [
        'version = 1',
        f'graph = "{config_dir / "eval.fabro"}"',
        '',
        '[pull_request]',
        'enabled = false',
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


def dot_escape(s: str) -> str:
    return s.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")


# ---------------------------------------------------------------------------
# Grade from test output
# ---------------------------------------------------------------------------


def grade_test_output(instance: dict, test_output: str) -> dict:
    """Grade test output using swebench's log parsers.

    Returns a dict with 'resolved' (bool), 'status' string, and details.
    """
    spec = make_test_spec(instance)

    # Write test output to a temp file for get_logs_eval
    with tempfile.NamedTemporaryFile(mode="w", suffix=".log", delete=False) as f:
        f.write(test_output)
        f.flush()
        log_path = f.name

    try:
        eval_status_map, patch_applied = get_logs_eval(spec, log_path)
    finally:
        Path(log_path).unlink(missing_ok=True)

    if not patch_applied:
        return {
            "resolved": False,
            "status": "patch_failed",
            "detail": "Patch did not apply or tests errored",
        }

    if not eval_status_map:
        return {
            "resolved": False,
            "status": "no_test_results",
            "detail": "Could not parse test results from output",
        }

    # Build gold results in the format expected by get_eval_tests_report
    gold = {
        "FAIL_TO_PASS": spec.FAIL_TO_PASS,
        "PASS_TO_PASS": spec.PASS_TO_PASS,
    }

    report = get_eval_tests_report(eval_status_map, gold)
    resolution = get_resolution_status(report)

    return {
        "resolved": resolution == "RESOLVED_FULL",
        "status": resolution,
        "f2p_total": len(spec.FAIL_TO_PASS),
        "p2p_total": len(spec.PASS_TO_PASS),
    }


# ---------------------------------------------------------------------------
# Per-instance evaluator
# ---------------------------------------------------------------------------


def evaluate_instance(
    instance: dict,
    model_patch: str,
    output_dir: Path,
    timeout: int,
) -> dict:
    """Evaluate a single instance by running tests in a Daytona sandbox."""
    instance_id = instance["instance_id"]
    config_dir = output_dir / "configs" / instance_id
    config_dir.mkdir(parents=True, exist_ok=True)

    result = {
        "instance_id": instance_id,
        "resolved": False,
        "status": "error",
        "error": None,
        "duration_s": 0,
    }

    if not model_patch.strip():
        result["status"] = "empty_patch"
        result["error"] = "No patch to evaluate"
        return result

    start_time = time.time()

    try:
        # Build eval script and encode for transport into sandbox
        eval_script = build_eval_script(instance, model_patch)
        (config_dir / "eval.sh").write_text(eval_script)
        b64 = base64.b64encode(eval_script.encode()).decode()

        # The script attr runs in the sandbox — decode and execute
        run_cmd = f"echo {b64} | base64 -d | bash"
        fabro_content = f'''digraph Eval {{
    rankdir=LR
    start [shape=Mdiamond]
    exit  [shape=Msquare]
    run_tests [label="Run Tests", shape=parallelogram, script="{dot_escape(run_cmd)}"]
    start -> run_tests -> exit
}}
'''
        (config_dir / "eval.fabro").write_text(fabro_content)

        toml_content = generate_eval_toml(instance, config_dir)
        toml_file = config_dir / "eval.toml"
        toml_file.write_text(toml_content)

        cmd = [
            "fabro", "run", str(toml_file),
            "--auto-approve",
            "--no-retro",
            "--label", f"swe-eval={instance_id}",
        ]

        log.debug(f"[{instance_id}] Starting eval")
        proc = subprocess.run(
            cmd,
            cwd="/tmp",
            timeout=timeout,
            capture_output=True,
            text=True,
        )

        # Find fabro run dir from stderr
        fabro_run_dir = None
        for line in proc.stderr.splitlines():
            stripped = line.strip()
            if stripped.startswith("Run:") and "/" in stripped:
                fabro_run_dir = Path(
                    stripped.split("Run:", 1)[1].strip().replace("~", str(Path.home()))
                )
                break

        if proc.returncode != 0:
            (config_dir / "fabro_stderr.log").write_text(proc.stderr)
            log.debug(f"[{instance_id}] fabro exit={proc.returncode}")

        # Always try to read test output — tests may exit non-zero
        # but stdout.log is still written by fabro
        test_output = ""
        if fabro_run_dir:
            nodes_dir = fabro_run_dir / "nodes"
            if nodes_dir.exists():
                for node_dir in nodes_dir.iterdir():
                    if node_dir.name.startswith("run_tests"):
                        stdout_log = node_dir / "stdout.log"
                        if stdout_log.exists():
                            test_output = stdout_log.read_text()

        if not test_output:
            result["status"] = "no_output"
            result["error"] = f"No test output captured (fabro exit={proc.returncode})"
        else:
            (config_dir / "test_output.log").write_text(test_output)
            try:
                grade = grade_test_output(instance, test_output)
                result["resolved"] = grade["resolved"]
                result["status"] = grade["status"]
                if "detail" in grade:
                    result["error"] = grade["detail"]
            except Exception as e:
                result["status"] = "grade_error"
                result["error"] = str(e)
                log.debug(f"[{instance_id}] Grading error: {e}", exc_info=True)

    except subprocess.TimeoutExpired:
        result["status"] = "timeout"
        result["error"] = f"Timed out after {timeout}s"
        _cleanup_sandbox(instance_id, "swe-eval")
    except Exception as e:
        result["error"] = str(e)
        log.debug(f"[{instance_id}] Exception: {e}")

    result["duration_s"] = round(time.time() - start_time, 1)
    return result


def _cleanup_sandbox(label_value: str, label_key: str):
    """Best-effort delete of orphaned Daytona sandbox after timeout."""
    try:
        ps = subprocess.run(
            ["fabro", "ps", "--label", f"{label_key}={label_value}", "--json"],
            capture_output=True, text=True, timeout=10,
        )
        runs = json.loads(ps.stdout) if ps.stdout.strip() else []
        for run in runs:
            run_id = run.get("run_id", "")
            if not run_id:
                continue
            sandbox_name = f"fabro-{run_id}"
            subprocess.run(
                ["daytona", "sandbox", "delete", sandbox_name],
                capture_output=True, timeout=15,
            )
            log.debug(f"[{label_value}] Deleted sandbox {sandbox_name}")
    except Exception as e:
        log.debug(f"[{label_value}] Sandbox cleanup failed (non-fatal): {e}")


# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------

DAYTONA_CPU_LIMIT = 500


def preflight_daytona(max_workers: int, sandbox_cpu: int):
    """Check that we have enough Daytona CPU headroom before starting."""
    needed = max_workers * sandbox_cpu
    buffer = 1.2

    used_cpus = 0
    try:
        result = subprocess.run(
            ["daytona", "sandbox", "list"],
            capture_output=True, text=True, timeout=10,
        )
        sandbox_count = len(re.findall(
            r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}',
            result.stdout,
        ))
        used_cpus = sandbox_count * sandbox_cpu
    except Exception:
        pass

    available = DAYTONA_CPU_LIMIT - used_cpus
    required = int(needed * buffer)

    if required > available:
        print(f"Preflight FAILED: need {required} CPUs "
              f"({max_workers} workers x {sandbox_cpu} CPU x {buffer} buffer) "
              f"but only {available} available "
              f"({DAYTONA_CPU_LIMIT} limit - {used_cpus} in use)")
        print(f"  Reduce --max-workers to {int(available / buffer / sandbox_cpu)} or fewer")
        sys.exit(1)

    print(f"Preflight OK: {required} CPUs needed, {available} available "
          f"({used_cpus} in use, {DAYTONA_CPU_LIMIT} limit)")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main():
    parser = argparse.ArgumentParser(
        description="Evaluate SWE-bench predictions using Daytona sandboxes"
    )
    parser.add_argument(
        "--predictions", type=Path, required=True,
        help="Path to predictions JSONL file",
    )
    parser.add_argument(
        "--output-dir", type=Path, required=True,
        help="Output directory for eval results",
    )
    parser.add_argument(
        "--max-workers", type=int, default=100,
        help="Max concurrent eval sandboxes (default 100)",
    )
    parser.add_argument(
        "--timeout", type=int, default=1200,
        help="Timeout per instance in seconds",
    )
    parser.add_argument(
        "--instance-ids", nargs="+",
        help="Evaluate only these instance IDs",
    )
    args = parser.parse_args()

    args.output_dir = args.output_dir.resolve()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    setup_logging(args.output_dir)

    # --- Preflight: check Daytona capacity --------------------------------
    preflight_daytona(args.max_workers, sandbox_cpu=4)

    log.info("=" * 64)
    log.info("SWE-bench Evaluation (Daytona)")
    log.info("=" * 64)
    log.info(f"  Predictions: {args.predictions}")
    log.info(f"  Output:      {args.output_dir}")
    log.info(f"  Workers:     {args.max_workers}")
    log.info(f"  Timeout:     {args.timeout}s")
    log.info("")

    # Load predictions
    predictions = {}
    with open(args.predictions) as f:
        for line in f:
            p = json.loads(line)
            predictions[p["instance_id"]] = p["model_patch"]
    log.info(f"  {len(predictions)} predictions loaded")

    # Filter by instance IDs if specified
    if args.instance_ids:
        id_set = set(args.instance_ids)
        predictions = {k: v for k, v in predictions.items() if k in id_set}
        log.info(f"  Filtered to {len(predictions)} instances")

    # Resume: skip already-evaluated instances
    completed_ids = load_completed_ids(args.output_dir)
    if completed_ids:
        before = len(predictions)
        predictions = {k: v for k, v in predictions.items() if k not in completed_ids}
        log.info(f"  {len(completed_ids)} already evaluated, {len(predictions)} remaining")

    # Load dataset instances
    log.info("Loading SWE-bench Lite dataset...")
    dataset = load_dataset("princeton-nlp/SWE-bench_Lite", split="test")
    instances_by_id = {dict(row)["instance_id"]: dict(row) for row in dataset}

    # Match predictions to instances
    eval_items = []
    for iid, patch in predictions.items():
        if iid in instances_by_id:
            eval_items.append((instances_by_id[iid], patch))
        else:
            log.warning(f"  Instance {iid} not found in dataset, skipping")

    total = len(eval_items)
    log.info(f"  {total} instances to evaluate")
    log.info("")

    # Run evaluations
    lock = threading.Lock()
    counters: Counter[str] = Counter()
    resolved_count = 0
    done_count = 0
    wall_start = time.time()

    results_file = args.output_dir / "eval_results.jsonl"

    log.info(f"Evaluating {total} instances (max {args.max_workers} concurrent)...")
    log.info("-" * 64)

    with ThreadPoolExecutor(max_workers=args.max_workers) as executor:
        futures = {
            executor.submit(
                evaluate_instance, inst, patch, args.output_dir, args.timeout,
            ): inst["instance_id"]
            for inst, patch in eval_items
        }

        with open(results_file, "a") as rf:
            for future in as_completed(futures):
                result = future.result()
                iid = result["instance_id"]
                status = result["status"]
                dur = result["duration_s"]
                resolved = result["resolved"]

                with lock:
                    counters[status] += 1
                    if resolved:
                        resolved_count += 1
                    done_count += 1
                    n = done_count

                    rf.write(json.dumps(result) + "\n")
                    rf.flush()

                resolved_mark = "RESOLVED" if resolved else status
                err_info = f"  err={result['error'][:80]}" if result.get("error") else ""
                elapsed = round(time.time() - wall_start)
                log.info(
                    f"[{n:3d}/{total}]  {resolved_mark:<16s}  {dur:6.0f}s  "
                    f"{iid}{err_info}"
                )

                if n % 10 == 0 or n == total:
                    pct = 100 * resolved_count / n if n > 0 else 0
                    log.info(
                        f"  --- progress: {n}/{total}  "
                        f"resolved={resolved_count} ({pct:.1f}%)  "
                        f"elapsed={elapsed}s ---"
                    )

    wall_duration = round(time.time() - wall_start, 1)

    # Recompute summary from the full results file (includes prior runs)
    all_counters: Counter[str] = Counter()
    all_resolved = 0
    all_total = 0
    repo_total: Counter[str] = Counter()
    repo_resolved: Counter[str] = Counter()
    with open(results_file) as f:
        for line in f:
            if not line.strip():
                continue
            r = json.loads(line)
            all_counters[r["status"]] += 1
            all_total += 1
            if r["resolved"]:
                all_resolved += 1
            parts = r["instance_id"].split("__")
            repo = f"{parts[0]}/{parts[1].rsplit('-', 1)[0]}" if len(parts) >= 2 else r["instance_id"]
            repo_total[repo] += 1
            if r["resolved"]:
                repo_resolved[repo] += 1

    pct = 100 * all_resolved / all_total if all_total > 0 else 0

    summary = {
        "total": all_total,
        "resolved": all_resolved,
        "resolved_pct": round(pct, 1),
        "status_counts": dict(all_counters),
        "wall_duration_s": wall_duration,
    }
    (args.output_dir / "summary.json").write_text(json.dumps(summary, indent=2))

    skipped = len(completed_ids)
    log.info("")
    log.info("=" * 64)
    log.info("FINAL RESULTS")
    log.info("=" * 64)
    if skipped:
        log.info(f"  Skipped:     {skipped} (already evaluated)")
        log.info(f"  This run:    {total}")
    log.info(f"  Total:       {all_total}")
    log.info(f"  Resolved:    {all_resolved} ({pct:.1f}%)")
    log.info(f"  Wall time:   {wall_duration}s")
    log.info("")
    log.info(f"  {'Repo':<35s}  {'Resolved':>8s}  {'Total':>6s}  {'Rate':>6s}")
    log.info(f"  {'-'*35}  {'-'*8}  {'-'*6}  {'-'*6}")
    for repo in sorted(repo_total):
        res = repo_resolved[repo]
        tot = repo_total[repo]
        rate = 100 * res / tot if tot > 0 else 0
        log.info(f"  {repo:<35s}  {res:>8d}  {tot:>6d}  {rate:>5.1f}%")
    log.info("")
    log.info(f"  Status breakdown: {dict(all_counters)}")
    log.info(f"  Results:     {results_file}")
    log.info(f"  Summary:     {args.output_dir / 'summary.json'}")
    log.info(f"  Full log:    {args.output_dir / 'eval_grade.log'}")
    log.info("=" * 64)


if __name__ == "__main__":
    main()
