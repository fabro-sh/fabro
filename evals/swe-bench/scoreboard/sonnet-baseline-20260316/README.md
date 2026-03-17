# sonnet-baseline-20260316

**Date:** 2026-03-16
**Model:** claude-sonnet-4-6 (anthropic)
**Fabro:** fabro 0.5.0 (4c6ad4d 2026-03-16)

## Description

Sonnet 4.6 baseline, default prompt, 2 CPU / 4 GB, 10min timeout

## Results

| Metric | Value |
|--------|-------|
| Instances | 300 |
| Patched | 281 (93.7%) |
| **Resolved** | **167 (55.7%)** |
| Total gen cost | $39.78 |
| Avg gen cost | $0.1326/instance |
| Gen wall time | 993.6s |
| Eval wall time | 642.3s |

## Per-repo breakdown

| Repo | Resolved | Total | Rate |
|------|----------|-------|------|
| astropy/astropy | 1 | 6 | 16.7% |
| django/django | 86 | 114 | 75.4% |
| matplotlib/matplotlib | 0 | 23 | 0.0% |
| mwaskom/seaborn | 2 | 4 | 50.0% |
| pallets/flask | 0 | 3 | 0.0% |
| psf/requests | 5 | 6 | 83.3% |
| pydata/xarray | 1 | 5 | 20.0% |
| pylint-dev/pylint | 0 | 6 | 0.0% |
| pytest-dev/pytest | 11 | 17 | 64.7% |
| scikit-learn/scikit-learn | 14 | 23 | 60.9% |
| sphinx-doc/sphinx | 0 | 16 | 0.0% |
| sympy/sympy | 47 | 77 | 61.0% |
