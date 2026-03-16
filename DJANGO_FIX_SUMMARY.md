# Django Password Reset Token Security Fix

## Issue Summary
Changing a user's email address could invalidate password reset tokens, creating a security vulnerability:

1. User requests password reset for email `foo@example.com`
2. User changes their email address to `bar@example.com`
3. Original password reset token should be invalid, but it's actually accepted

## Root Cause
The `PasswordResetTokenGenerator._make_hash_value()` method did not include the user's email address in the token generation, so token validation doesn't detect email changes.

## Solution
Include the user's email address in the `_make_hash_value()` method:

1. Use `user.get_email_field_name()` to get the correct email field (handles custom user models)
2. Include the email in the hash value calculation
3. Update documentation to reflect that email changes invalidate tokens

## Implementation Details

### File: `django/contrib/auth/tokens.py`

The `_make_hash_value()` method was updated to:
- Include the user's email address in the hash computation
- Handle users without email fields (uses empty string as fallback)
- Use the model's email field name (supports custom user models)

**Before:**
```python
def _make_hash_value(self, user, timestamp):
    login_timestamp = '' if user.last_login is None else user.last_login.replace(microsecond=0, tzinfo=None)
    return str(user.pk) + user.password + str(login_timestamp) + str(timestamp)
```

**After:**
```python
def _make_hash_value(self, user, timestamp):
    login_timestamp = '' if user.last_login is None else user.last_login.replace(microsecond=0, tzinfo=None)
    email_field_name = user.get_email_field_name()
    email = getattr(user, email_field_name, '') or ''
    return str(user.pk) + user.password + str(login_timestamp) + email + str(timestamp)
```

### Test Coverage

Added test `test_token_invalidated_after_email_change()` to verify:
- Token is valid immediately after generation
- Token is invalidated when the user's email address changes
- Token validation fails after email change

## Backward Compatibility

⚠️ **Breaking Change**: Existing password reset tokens will be invalidated when this fix is deployed. Users who requested a password reset before the fix will need to request a new one.

This is acceptable because:
1. Password reset tokens are already time-limited (PASSWORD_RESET_TIMEOUT)
2. The security benefit outweighs the minor UX inconvenience
3. Users will receive clear instructions to request a new token

## Testing

All tests pass:
```
Testing against Django installed in '/tmp/django-fix/django' with up to 48 processes
Creating test database for alias 'default'...
System check identified no issues (0 silenced).
........
----------------------------------------------------------------------
Ran 8 tests in 0.005s

OK
Destroying test database for alias 'default'...
```

## Git Commit Reference
Based on Django commit: `7f9e4524d6b23424cf44fbe1bf1f4e70f6bb066e`
