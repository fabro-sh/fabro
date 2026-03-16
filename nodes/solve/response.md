All tests pass. Here's a summary of the changes made:

## Changes

### `django/db/models/fields/__init__.py`

1. **Added `_check_max_length_choices` call to `CharField.check()`** — Includes the new check in the CharField system checks pipeline.

2. **Added `CharField._check_max_length_choices()` method** — A new system check that:
   - Only runs when both `max_length` and `choices` are set
   - Uses `self.flatchoices` to handle both flat choices and grouped choices
   - Filters to only string values (non-string choice values like integers don't have a length constraint)
   - Gracefully handles invalid choices (TypeError/ValueError) since those are already caught by `_check_choices()`
   - Returns `fields.E122` error when the longest choice value exceeds `max_length`

### `tests/invalid_models_tests/test_ordinary_fields.py`

Added two test cases:
- **`test_choices_longer_than_max_length`** — Verifies the error is raised for flat choices that exceed max_length
- **`test_choices_in_groups_longer_than_max_length`** — Verifies the error is raised for grouped choices that exceed max_length