# Fix: UsernameValidator allows trailing newline in usernames

## Problem
The `ASCIIUsernameValidator` and `UnicodeUsernameValidator` classes in `django/contrib/auth/validators.py` used regex patterns with `^` and `$` anchors, which have a quirk in Python: `$` matches both the end of string AND before a trailing newline character.

This caused the validators to incorrectly accept usernames ending with a newline (e.g., `"username\n"`).

## Root Cause
The original regex patterns were:
```python
regex = r'^[\w.@+-]+$'
```

The `$` anchor in Python regex matches:
1. The absolute end of string
2. **Also** before a trailing newline character (POSIX behavior)

## Solution
Changed the regex patterns to use `\A` and `\Z` anchors instead:
```python
regex = r'\A[\w.@+-]+\Z'
```

Where:
- `\A` - matches only the absolute start of string (not after newlines)
- `\Z` - matches only the absolute end of string (not before newlines)

## Changes Made
Modified two validator classes in `django/contrib/auth/validators.py`:

1. **ASCIIUsernameValidator** (line 10):
   - From: `regex = r'^[\w.@+-]+$'`
   - To: `regex = r'\A[\w.@+-]+\Z'`

2. **UnicodeUsernameValidator** (line 20):
   - From: `regex = r'^[\w.@+-]+$'`
   - To: `regex = r'\A[\w.@+-]+\Z'`

## Testing
Tested with both regexes to verify:

### OLD regex behavior (`^[\w.@+-]+$`):
```
'validuser'        -> MATCH ✓ (correct)
'user-name'        -> MATCH ✓ (correct)
'validuser\n'      -> MATCH ✗ (BUG - should not match)
'user-name\n'      -> MATCH ✗ (BUG - should not match)
'user with space'  -> NO MATCH ✓ (correct)
'user!invalid'     -> NO MATCH ✓ (correct)
```

### NEW regex behavior (`\A[\w.@+-]+\Z`):
```
'validuser'        -> MATCH ✓ (correct)
'user-name'        -> MATCH ✓ (correct)
'validuser\n'      -> NO MATCH ✓ (FIXED)
'user-name\n'      -> NO MATCH ✓ (FIXED)
'user with space'  -> NO MATCH ✓ (correct)
'user!invalid'     -> NO MATCH ✓ (correct)
```

## Impact
- **Security**: Prevents acceptance of usernames with trailing newlines, which could bypass validation in some contexts
- **Correctness**: Makes the validators behave as intended, accepting only alphanumeric characters plus `.@+-_`
- **Compatibility**: No breaking changes - all previously valid usernames remain valid; only invalid usernames (with trailing newlines) are now properly rejected
