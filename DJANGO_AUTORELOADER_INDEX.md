# Django Issue #30479 - Complete Solution Index

## Problem Statement

**Django Issue #30479**: Autoreloader with StatReloader doesn't track changes in `manage.py`

- **Affected Version**: Django 2.2.1
- **Regression From**: Django 2.1.8
- **Root Cause**: `__main__` module has `__spec__ = None`, causing autoreloader to skip it
- **Solution**: Add special case handling for `__main__` module

## Quick Navigation

### For Those in a Hurry ⏱️
1. Read: [DJANGO_AUTORELOADER_FIX_README.md](DJANGO_AUTORELOADER_FIX_README.md) (5 min read)
2. Apply: `git apply django-autoreloader-30479.patch`
3. Done! ✓

### For Detailed Understanding 📚
1. Start: [DJANGO_ISSUE_FIX.md](DJANGO_ISSUE_FIX.md) - Root cause and solution
2. Review: [ISSUE_RESOLUTION_SUMMARY.md](ISSUE_RESOLUTION_SUMMARY.md) - Executive overview
3. Apply: [django-autoreloader-30479.patch](django-autoreloader-30479.patch) - The actual fix

### For Automation 🤖
- Workflow: [test/django_autoreload.fabro](test/django_autoreload.fabro) - Fabro workflow definition
- Use with: `fabro run django_autoreload` (if Fabro is installed)

## File Descriptions

| File | Purpose | Audience |
|------|---------|----------|
| [DJANGO_AUTORELOADER_FIX_README.md](DJANGO_AUTORELOADER_FIX_README.md) | User guide and getting started | Everyone |
| [DJANGO_ISSUE_FIX.md](DJANGO_ISSUE_FIX.md) | Technical documentation | Developers |
| [django-autoreloader-30479.patch](django-autoreloader-30479.patch) | Git patch file | Git users |
| [ISSUE_RESOLUTION_SUMMARY.md](ISSUE_RESOLUTION_SUMMARY.md) | Executive summary | Managers/Leads |
| [test/django_autoreload.fabro](test/django_autoreload.fabro) | Workflow definition | Fabro users |

## The Fix at a Glance

### Problem
```python
# In django/utils/autoreload.py
for module in modules:
    spec = getattr(module, "__spec__", None)
    if spec is None:
        continue  # ← __main__ (manage.py) is skipped here!
```

### Solution
```python
# In django/utils/autoreload.py
for module in modules:
    # Special case for __main__ module (e.g., manage.py)
    if module.__name__ == '__main__':
        if hasattr(module, '__file__') and module.__file__:
            yield module.__file__
        continue
    
    spec = getattr(module, "__spec__", None)
    if spec is None:
        continue
```

### Impact
- **Lines changed**: ~5 lines
- **Files modified**: 1 file
- **Breaking changes**: None
- **Backwards compatible**: Yes
- **Performance impact**: Negligible

## How to Apply

### Method 1: Using Git Apply (Recommended)
```bash
cd /path/to/django
git apply django-autoreloader-30479.patch
```

### Method 2: Manual Edit
1. Open `django/utils/autoreload.py`
2. Find function `iter_modules_and_files()`
3. Add the special case check for `__main__` before the `spec = getattr(...)` line
4. Save and test

### Method 3: Using Fabro
```bash
fabro run django_autoreload
```

## Testing

### Before Applying Fix
```bash
# Django 2.2.1 (with bug)
python manage.py runserver
# Edit manage.py → Server does NOT restart ❌
```

### After Applying Fix
```bash
# Django 2.2.1 (with fix applied)
python manage.py runserver
# Edit manage.py → Server restarts automatically ✓
```

### Automated Testing
```bash
python -m pytest tests/autoreload/ -xvs
```

## Commit History

```
24aed50 - docs: Add comprehensive user guide for Django autoreloader fix
8773b58 - docs: Add comprehensive issue resolution summary
04cd18f - fix: Add patch for Django issue #30479 - StatReloader doesn't track manage.py
8762607 - docs: Add detailed implementation guide for Django autoreloader fix
5c87d24 - docs: Add Django autoreloader issue #30479 documentation and test workflow
```

## Django Contribution Path

Want to submit this fix to Django directly?

1. Fork [django/django](https://github.com/django/django)
2. Create branch: `git checkout -b ticket-30479-autoreload-main`
3. Apply patch: `git apply django-autoreloader-30479.patch`
4. Run tests: `python runtests.py tests/autoreload`
5. Commit: `git commit -m "Fixed #30479 -- Autoreloader tracks __main__ module changes."`
6. Create pull request with reference to ticket #30479

## Related Resources

- **Django Ticket**: [code.djangoproject.com/ticket/30479](https://code.djangoproject.com/ticket/30479)
- **Django Repository**: [github.com/django/django](https://github.com/django/django)
- **Python __main__ Docs**: [docs.python.org/3/library/__main__.html](https://docs.python.org/3/library/__main__.html)

## Key Points

✅ **What's Fixed**: Editing `manage.py` now triggers autoreloader restart  
✅ **How it works**: Special case handling for `__main__` module  
✅ **Why it works**: Checks `__file__` attribute when `__spec__` is None  
✅ **Where it applies**: `django/utils/autoreload.py`  
✅ **Backward compatibility**: 100% compatible  
✅ **Performance**: No measurable impact  

## Support & Questions

For questions about:
- **The fix itself**: See [DJANGO_ISSUE_FIX.md](DJANGO_ISSUE_FIX.md)
- **How to apply it**: See [DJANGO_AUTORELOADER_FIX_README.md](DJANGO_AUTORELOADER_FIX_README.md)
- **The workflow**: See [test/django_autoreload.fabro](test/django_autoreload.fabro)
- **Django specifics**: Visit [Django Issue Tracker](https://code.djangoproject.com/)

---

**Status**: ✅ Complete and Ready  
**Last Updated**: 2026-03-16  
**Validation**: Tested against Django 2.2.1
