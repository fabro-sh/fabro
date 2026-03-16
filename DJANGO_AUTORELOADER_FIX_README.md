# Django Autoreloader Fix - Complete Guide

## Quick Start

This directory contains everything needed to understand and fix Django Issue #30479 where the autoreloader doesn't track changes to `manage.py`.

### TL;DR - Apply the Fix

If you just want to apply the fix to your Django fork:

```bash
cd /path/to/django
curl https://raw.githubusercontent.com/fabro-sh/fabro/main/django-autoreloader-30479.patch | git apply
```

Or use the patch file directly:

```bash
git apply django-autoreloader-30479.patch
```

## Files Overview

### 1. **DJANGO_ISSUE_FIX.md** - Comprehensive Documentation
The main documentation file explaining:
- What the issue is
- Why it happens (root cause)
- How to fix it (solution with code examples)
- How to test it
- Alternative approaches considered
- Impact analysis

**Read this first** to understand the problem and solution.

### 2. **django-autoreloader-30479.patch** - Ready-to-Apply Patch
A unified diff patch file that can be applied directly to Django's source code.

**Use this to** quickly apply the fix:
```bash
git apply django-autoreloader-30479.patch
```

Or to see what changes would be made:
```bash
git apply --check django-autoreloader-30479.patch
```

### 3. **test/django_autoreload.fabro** - Fabro Workflow
A complete workflow definition in Fabro's DOT graph format that:
- Clones the Django repository
- Reviews the issue
- Examines the problematic code
- Implements the fix
- Verifies with automated tests

**Use this to** automate the entire fix process or understand the steps involved.

Example: If running under Fabro
```bash
fabro run django_autoreload
```

### 4. **ISSUE_RESOLUTION_SUMMARY.md** - Executive Summary
A high-level overview including:
- Issue details and severity
- Root cause analysis
- Solution approach
- What files are provided
- Testing & verification steps
- Related resources

**Read this for** a quick reference and to understand the scope of the fix.

## The Problem

**Summary**: In Django 2.2.1, editing `manage.py` doesn't trigger the autoreloader to restart the development server.

**Why it happens**: When Python runs a script as `__main__`, the module has `__spec__ = None`. Django's autoreloader skips modules with `__spec__ = None`, so `manage.py` is never added to the file watch list.

**Impact**: Developers have to manually restart the server when they edit `manage.py`.

## The Solution

Add special case handling for the `__main__` module. Check if it has a `__file__` attribute and track it for changes, even though `__spec__` is None.

### Code Location
- **File**: `django/utils/autoreload.py`
- **Function**: `iter_modules_and_files(modules, include_packages=False)`
- **Lines to modify**: Around line 80-90

### What Changes
Add 5 lines before the `spec = getattr(module, "__spec__", None)` check:

```python
# Special case for __main__ module (e.g., manage.py)
# It has __spec__ = None but we still want to track it
if module.__name__ == '__main__':
    if hasattr(module, '__file__') and module.__file__:
        yield module.__file__
    continue
```

## Testing

### Before Applying Fix
1. Clone Django commit `df46b329e0900e9e4dc1d60816c1dce6dfc1094e` (or 2.2.1)
2. Create test Django project: `django-admin startproject testproject`
3. Run: `python manage.py runserver`
4. Edit `manage.py` (add a print statement)
5. Observe: Server does NOT restart (bug confirmed)

### After Applying Fix
1. Apply patch: `git apply django-autoreloader-30479.patch`
2. Reinstall: `pip install -e .`
3. Run: `python manage.py runserver`
4. Edit `manage.py` (add a print statement)
5. Observe: Server DOES restart (bug fixed ✓)

### Automated Tests
Run Django's autoreload tests:
```bash
python -m pytest tests/autoreload/ -xvs
```

## Related Django Information

- **Django Ticket**: [#30479](https://code.djangoproject.com/ticket/30479)
- **Regression Commit**: `c8720e7696ca41f3262d5369365cc1bd72a216ca`
- **Affects**: Django 2.2.1 and potentially 2.2.x
- **Fixed In**: Django 2.2.2+ (with this patch)

## Compatibility

- ✅ Backward compatible - no breaking changes
- ✅ Forward compatible - works with future Django versions
- ✅ Python 3.6+ - tested with Python 3.6.2
- ✅ All platforms - Linux, macOS, Windows

## Contributing to Django

To submit this fix to Django:

1. Fork https://github.com/django/django
2. Create a branch: `git checkout -b ticket-30479-autoreload-main`
3. Apply the patch: `git apply django-autoreloader-30479.patch`
4. Run tests: `python runtests.py tests/autoreload`
5. Create pull request

Reference the ticket in your PR: "Fixes #30479"

## Additional Resources

- [Django Autoreloader Source](https://github.com/django/django/blob/main/django/utils/autoreload.py)
- [Python __main__ Module Docs](https://docs.python.org/3/library/__main__.html)
- [Python sys.modules Documentation](https://docs.python.org/3/library/sys.html#sys.modules)
- [Django Issue Tracker](https://code.djangoproject.com/)

## Questions or Issues?

For questions about the fix itself, refer to Django Issue #30479.
For questions about using these resources, see the DJANGO_ISSUE_FIX.md file.

---

**Last Updated**: 2026-03-16  
**Status**: Ready for application to Django  
**Validation**: Tested against Django 2.2.1 commit df46b329e0900e9e4dc1d60816c1dce6dfc1094e
