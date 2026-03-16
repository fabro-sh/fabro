Implemented the Django fix for this issue.

What changed:
- `django/db/models/fields/__init__.py`
  - Added a system check in `Field._check_choices()` to compute the longest string choice value and raise `fields.E009` when `max_length` is too small.
  - Kept it safe for grouped choices with non-string values.
- `tests/invalid_models_tests/test_ordinary_fields.py`
  - Added regression tests for both flat and grouped `choices` where a choice value exceeds `max_length`.

Validation:
- `python -m pip install -e .`
- `python tests/runtests.py invalid_models_tests.test_ordinary_fields --parallel 1`

Notes:
- The provided setup failed because the workspace wasn't empty, so I overlaid the Django source at the requested commit into the workspace before applying the fix.