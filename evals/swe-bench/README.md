# SWE-Bench-Lite Evaluation

Evaluates Fabro's agent on [SWE-Bench-Lite](https://www.swebench.com/) (300 Python bug-fix tasks across 12 repos). Two phases: generate patches, then evaluate them. Both run on Daytona cloud sandboxes — no local Docker needed.

## Setup

```bash
cd evals/swe-bench
python3 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
```

## Step 1: Generate patches

Runs Fabro's agent on each SWE-bench instance to produce a fix.

```bash
python run_eval.py \
    --model claude-haiku-4-5 \
    --provider anthropic \
    --output-dir results/haiku-baseline \
    2>&1 | tee results/haiku-baseline/console.log
```

**Options:**
- `--model` — LLM model (default: `claude-haiku-4-5`)
- `--provider` — LLM provider (default: `anthropic`)
- `--max-workers` — max concurrent Daytona sandboxes (default: 100)
- `--timeout` — per-instance timeout in seconds (default: 600)
- `--instance-ids` — run only specific instances (e.g. `--instance-ids django__django-11099`)

**Monitor:**
```bash
python status.py results/haiku-baseline    # quick summary
tail -f results/haiku-baseline/eval.log    # live per-instance results
fabro ps                                   # active sandboxes
fabro logs <RUN_ID>                        # stream a specific run
```

## Step 2: Evaluate patches

Applies each patch, runs the held-out test suite, and grades pass/fail using swebench's log parsers.

```bash
python evaluate_daytona.py \
    --predictions results/haiku-baseline/predictions.jsonl \
    --output-dir results/haiku-baseline/eval \
    2>&1 | tee results/haiku-baseline/eval/console.log
```

**Options:**
- `--max-workers` — max concurrent eval sandboxes (default: 100)
- `--timeout` — per-instance timeout in seconds (default: 600)
- `--instance-ids` — evaluate only specific instances

**Monitor:**
```bash
python status.py results/haiku-baseline/eval   # quick summary
tail -f results/haiku-baseline/eval/eval_grade.log
```

## Step 3: Record results

Saves results to the git-tracked `scoreboard/` directory for permanent record-keeping.

```bash
python record_results.py \
    --run-name haiku-baseline-20260316 \
    --gen-dir results/haiku-baseline \
    --eval-dir results/haiku-baseline/eval \
    --description "Haiku 4.5 baseline, default prompt, 2 CPU / 4 GB, 10min timeout"
```

Then commit the scoreboard:
```bash
git add scoreboard/
git commit -m "Record haiku-baseline-20260316: XX.X% on SWE-Bench-Lite"
```

## Scoreboard

Results are stored in `scoreboard/`:

```
scoreboard/
├── leaderboard.json                    # all runs ranked by resolve rate
└── haiku-baseline-20260316/
    ├── README.md                       # human-readable summary
    ├── meta.json                       # run metadata, costs, per-repo stats
    └── instances.jsonl                 # per-instance: has_patch, resolved, duration, cost
```

View the leaderboard:
```bash
cat scoreboard/leaderboard.json | python3 -m json.tool
```

## File inventory

| File | Purpose |
|------|---------|
| `status.py` | Check progress of a running or completed generation/evaluation |
| `run_eval.py` | Generate patches (step 1) |
| `evaluate_daytona.py` | Evaluate patches on Daytona (step 2) |
| `evaluate.py` | Evaluate patches via official swebench Docker harness (alternative to step 2) |
| `record_results.py` | Record results to scoreboard (step 3) |
| `gen_dockerfile.py` | Generate per-(repo, version) Dockerfiles from swebench specs |
| `workflow.fabro` | DOT workflow template (unused — per-instance .fabro files are generated) |
| `requirements.txt` | Python dependencies: `swebench`, `datasets` |
| `scoreboard/` | Git-tracked results (committed) |
| `results/` | Raw run data — predictions, logs, patches (gitignored) |

## Running a new model

Full end-to-end for a new model:

```bash
# 1. Generate
python run_eval.py \
    --model claude-opus-4-6 --provider anthropic \
    --output-dir results/opus-baseline \
    2>&1 | tee results/opus-baseline/console.log

# 2. Evaluate
python evaluate_daytona.py \
    --predictions results/opus-baseline/predictions.jsonl \
    --output-dir results/opus-baseline/eval \
    2>&1 | tee results/opus-baseline/eval/console.log

# 3. Record
python record_results.py \
    --run-name opus-baseline-20260316 \
    --gen-dir results/opus-baseline \
    --eval-dir results/opus-baseline/eval \
    --description "Opus 4.6 baseline, default prompt, 2 CPU / 4 GB, 10min timeout"

# 4. Commit
git add scoreboard/
git commit -m "Record opus-baseline-20260316"
```

## Sandbox resources

Each Daytona sandbox uses 2 CPU / 4 GB RAM / 10 GB disk. Snapshots are cached by name — first build is slow (~2 min), subsequent uses are instant.
