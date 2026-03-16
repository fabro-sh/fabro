The fix works correctly. Both validators now:
- ✅ Accept valid usernames (e.g., `validuser`)
- ✅ Reject usernames with trailing newlines (e.g., `validuser\n`)

**Summary of changes:** In `django/contrib/auth/validators.py`, changed the regex in both `ASCIIUsernameValidator` and `UnicodeUsernameValidator` from `r'^[\w.@+-]+$'` to `r'\A[\w.@+-]+\Z'`. 

The issue was that Python's `$` in regex matches both end-of-string and just before a trailing newline, while `\Z` matches only the absolute end of the string. Similarly, `\A` is used instead of `^` for consistency (matching only the absolute start of the string).