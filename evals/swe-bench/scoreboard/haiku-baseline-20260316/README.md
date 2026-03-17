# haiku-baseline-20260316

**Date:** 2026-03-16
**Model:** claude-haiku-4-5 (anthropic)
**Fabro:** fabro 0.5.0 (4c6ad4d 2026-03-16)

## Description

Haiku 4.5 baseline, default prompt, 2 CPU / 4 GB, 10min timeout

## Results

| Metric | Value |
|--------|-------|
| Instances | 300 |
| Patched | 293 (97.7%) |
| **Resolved** | **162 (54.0%)** |
| Total gen cost | $26.13 |
| Avg gen cost | $0.0871/instance |
| Gen wall time | 3559.2s |
| Eval wall time | 625.2s |

## Per-repo breakdown

| Repo | Resolved | Total | Rate |
|------|----------|-------|------|
| astropy/astropy | 1 | 6 | 16.7% |
| django/django | 86 | 114 | 75.4% |
| matplotlib/matplotlib | 0 | 23 | 0.0% |
| mwaskom/seaborn | 1 | 4 | 25.0% |
| pallets/flask | 0 | 3 | 0.0% |
| psf/requests | 4 | 6 | 66.7% |
| pydata/xarray | 1 | 5 | 20.0% |
| pylint-dev/pylint | 0 | 6 | 0.0% |
| pytest-dev/pytest | 11 | 17 | 64.7% |
| scikit-learn/scikit-learn | 12 | 23 | 52.2% |
| sphinx-doc/sphinx | 0 | 16 | 0.0% |
| sympy/sympy | 46 | 77 | 59.7% |
