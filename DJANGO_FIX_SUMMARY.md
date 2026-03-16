# Django Admin Inline verbose_name_plural Fix

## Issue
Django's Admin Inline classes did not automatically derive `verbose_name_plural` from `verbose_name`, unlike Django's Model Meta class. This was inconsistent and required developers to explicitly set both values if they wanted to override the default verbose name.

## Solution
Modified the `InlineModelAdmin.__init__` method to make `verbose_name_plural` automatically fallback to the pluralized form of `verbose_name` (if specified), consistent with how Model Meta options work.

## Changes Made

### 1. Code Implementation (django/contrib/admin/options.py)

The key change is reordering the initialization logic:

**Before:**
```python
if self.verbose_name is None:
    self.verbose_name = self.model._meta.verbose_name
if self.verbose_name_plural is None:
    self.verbose_name_plural = self.model._meta.verbose_name_plural
```

**After:**
```python
if self.verbose_name_plural is None:
    if self.verbose_name is None:
        self.verbose_name_plural = self.model._meta.verbose_name_plural
    else:
        self.verbose_name_plural = format_lazy('{}s', self.verbose_name)
if self.verbose_name is None:
    self.verbose_name = self.model._meta.verbose_name
```

This ensures:
- If `verbose_name_plural` is explicitly set → use it as-is
- If `verbose_name` is explicitly set but `verbose_name_plural` is not → derive plural by adding 's'
- If neither are set → use model's default values

### 2. Test Implementation (tests/admin_inlines/tests.py)

Added `test_verbose_name_inline()` test to verify the behavior:
- Tests Inline classes with only `verbose_name` specified
- Verifies that `verbose_name_plural` is correctly pluralized
- Tests work with different model configurations

All existing tests continue to pass, including `test_verbose_name_plural_inline()` which tests explicit `verbose_name_plural` settings.

### 3. Documentation Updates

**docs/ref/contrib/admin/index.txt:**
- Updated the `InlineModelAdmin.verbose_name_plural` attribute documentation
- Clearly documented the fallback behavior
- Added version changed note (Django 4.0)

**docs/releases/4.0.txt:**
- Added minor feature note about the new fallback behavior

## Testing

All tests pass successfully:
```
$ python tests/runtests.py admin_inlines --verbosity=1
Ran 76 tests - OK (skipped=12)
```

Specific verbose_name tests:
```
$ python tests/runtests.py admin_inlines -k test_verbose_name --verbosity=2
test_verbose_name_inline ... ok
test_verbose_name_plural_inline ... ok
```

## Backwards Compatibility

This change is fully backwards compatible:
- Existing code that explicitly sets both `verbose_name` and `verbose_name_plural` continues to work unchanged
- Existing code that sets only `verbose_name_plural` continues to work unchanged
- This only changes behavior for code that sets only `verbose_name` - which previously would not have a proper plural form anyway

## PR Ready
The implementation is complete and ready to be submitted as a Django pull request. The changes are minimal, focused, and well-tested.
