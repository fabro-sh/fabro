# Django DurationField Error Message Format Correction

## Issue
The `DurationField` error message in Django incorrectly displayed the duration format. When users entered an invalid duration, they received an error message stating the format should be:

```
[DD] [HH:[MM:]]ss[.uuuuuu]
```

However, this format specification is **incorrect**. According to the actual behavior of the `parse_duration()` function, the correct format should be:

```
[DD] [[HH:]MM:]ss[.uuuuuu]
```

## Analysis

### Format Breakdown

Looking at the `standard_duration_re` regex pattern in `django/utils/dateparse.py`:

```python
standard_duration_re = re.compile(
    r'^'
    r'(?:(?P<days>-?\d+) (days?, )?)?'      # Optional: days
    r'(?P<sign>-?)'                           # Optional: sign
    r'((?:(?P<hours>\d+):)(?=\d+:\d+))?'     # Optional: hours (only if MM:ss follows)
    r'(?:(?P<minutes>\d+):)?'                 # Optional: minutes
    r'(?P<seconds>\d+)'                       # REQUIRED: seconds
    r'(?:\.(?P<microseconds>\d{1,6})\d{0,6})?'  # Optional: microseconds
    r'$'
)
```

### Key Differences

| Component | Status | Old Format | New Format |
|-----------|--------|-----------|-----------|
| Days | Optional | `[DD]` | `[DD]` |
| Hours | Optional (only with MM) | `[HH:[MM:]]` | `[[HH:]MM:]` |
| Minutes | Optional | Implied in Hours | Explicit as `MM` |
| Seconds | **MANDATORY** | `ss` | `ss` |
| Microseconds | Optional | `[.uuuuuu]` | `[.uuuuuu]` |

### Why the Difference?

The old format `[HH:[MM:]]ss` implies that:
- Hours are optional
- If hours are present, minutes are optional
- But this doesn't clearly show that **minutes are required if hours are present**

The new format `[[HH:]MM:]ss` correctly shows that:
- Hours and minutes form an optional group: `[[HH:]MM:]`
- Minutes MUST be present if hours are present
- Seconds are always required

### Valid Examples

All these formats are valid according to the regex:
- `14` → 14 seconds
- `14:00` → 14 minutes, 0 seconds
- `1:14:00` → 1 hour, 14 minutes, 0 seconds
- `1 1:14:00` → 1 day, 1 hour, 14 minutes, 0 seconds
- `14:00:00.123456` → 14 hours, 0 minutes, 0 seconds, 123456 microseconds
- `10:30` → 10 minutes, 30 seconds

## Solution

The fix involves updating the error message format string in **two files**:

### 1. `django/db/models/fields/__init__.py` (Line 1590)

**Before:**
```python
default_error_messages = {
    'invalid': _("'%(value)s' value has an invalid format. It must be in "
                 "[DD] [HH:[MM:]]ss[.uuuuuu] format.")
}
```

**After:**
```python
default_error_messages = {
    'invalid': _("'%(value)s' value has an invalid format. It must be in "
                 "[DD] [[HH:]MM:]ss[.uuuuuu] format.")
}
```

### 2. `tests/model_fields/test_durationfield.py` (Line 78)

**Before:**
```python
"It must be in [DD] [HH:[MM:]]ss[.uuuuuu] format."
```

**After:**
```python
"It must be in [DD] [[HH:]MM:]ss[.uuuuuu] format."
```

## Impact

- **Minimal change**: Only 2 lines modified in 2 files
- **User-facing**: The error message users receive will now accurately reflect the accepted duration format
- **Backward compatibility**: No code behavior changes, only the error message text
- **Test update**: The test validates that the error message matches the implementation

## Verification

The fix has been verified by:
1. Confirming the regex pattern in `django/utils/dateparse.py`
2. Testing multiple valid duration formats against the regex
3. Ensuring all test cases pass with the updated error message
