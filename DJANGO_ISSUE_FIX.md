# Django Autoreloader Issue Fix: manage.py Not Tracked

## Issue Summary

In Django 2.2.1, the autoreloader using `StatReloader` does not track changes to `manage.py`, preventing automatic server restart when the main file is edited. This is a regression from Django 2.1.8.

## Root Cause

The issue is in Django's `django.utils.autoreload` module, specifically in the `iter_modules_and_files()` function. When `manage.py` is run as the main module:

1. It appears in `sys.modules` as `__main__`
2. The `__main__` module has `__spec__ = None` (by design in Python)
3. The autoreloader code checks `getattr(module, "__spec__", None)` and skips modules where it's None
4. This causes `manage.py` to never be added to the list of files to watch

## Solution

Add special case handling for the `__main__` module. Even though `__spec__` is None, we can still retrieve the module's filename via `__file__` attribute.

### Code Changes

In `django/utils/autoreload.py`, the `iter_modules_and_files()` function should be modified to:

```python
def iter_modules_and_files(modules, include_packages=False):
    """
    Iterate over all modules and files to watch for changes.
    """
    for module in modules:
        if module.__name__ == '__main__':
            # Special case: __main__ has __spec__ = None but has __file__
            if hasattr(module, '__file__') and module.__file__:
                yield module.__file__
        else:
            # ... rest of existing logic ...
            spec = getattr(module, '__spec__', None)
            if spec is None:
                continue
            # ... rest of existing logic ...
```

## Testing

To verify the fix works:

1. Create a Django project with `python manage.py startproject myproject`
2. Run `python manage.py runserver`
3. Edit `manage.py` to add a comment or print statement
4. The server should automatically restart
5. Verify the change takes effect without manual restart

## Related Django Commits

- Regression introduced in: `c8720e7696ca41f3262d5369365cc1bd72a216ca`
- Issue reproduced at: `8d010f39869f107820421631111417298d1c5bb9`

## Detailed Implementation

The fix should be implemented in `django/utils/autoreload.py` in the file watching logic:

### Original Code Problem

```python
def iter_modules_and_files(modules, include_packages=False):
    """Iterate over all modules and yield their filenames."""
    for module in modules:
        spec = getattr(module, "__spec__", None)
        if spec is None:
            continue  # <- __main__ is skipped here!
        # ... rest of code
```

### Fixed Code

```python
def iter_modules_and_files(modules, include_packages=False):
    """Iterate over all modules and yield their filenames."""
    for module in modules:
        # Special case for __main__ which has __spec__ = None
        if module.__name__ == '__main__':
            if hasattr(module, '__file__') and module.__file__:
                yield module.__file__
            continue
        
        spec = getattr(module, "__spec__", None)
        if spec is None:
            continue
        # ... rest of existing code
```

## Alternative Approaches Considered

1. **Modify Python's `__main__` module**: Not feasible - this is standard Python behavior
2. **Track all modules including None specs**: Could cause performance issues and watch unnecessary files
3. **Special case __main__ in autoreloader**: ✓ Best approach - minimal, targeted fix

## Impact Analysis

- **Files modified**: 1 file (`django/utils/autoreload.py`)
- **Lines changed**: ~5-10 lines
- **Backwards compatible**: Yes
- **Performance impact**: Negligible (only one additional module check)
- **Risk**: Very low (special case handling only)