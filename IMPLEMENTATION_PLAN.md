# Implementation Plan: Fix Django Password Reset Token Vulnerability

## Problem Statement
When a Django user requests a password reset token, the token is generated based on:
- User's primary key
- User's password
- User's last login timestamp
- Request timestamp

The issue is that if a user changes their email address before using the password reset token, the token remains valid because the email is not part of the token validation hash.

**Vulnerability Sequence:**
1. User with email `foo@example.com` requests password reset
2. Password reset token is generated (e.g., `ABC123`)
3. User changes their email to `bar@example.com`
4. User uses the token `ABC123` to reset their password
5. Token is accepted (SECURITY FLAW!)

The token should be invalidated at step 5 since the email changed.

## Solution Overview
Include the user's email address in the token generation hash so that email changes invalidate existing tokens.

## Implementation Steps

### Step 1: Modify `django/contrib/auth/tokens.py`

**Change Location:** `PasswordResetTokenGenerator._make_hash_value()`

**What to change:**
- Add the user's email to the hash value computation
- Use `user.get_email_field_name()` to get the email field name (supports custom user models)
- Handle cases where a user might not have an email field

**Code Change:**
```python
def _make_hash_value(self, user, timestamp):
    """
    Hash the user's primary key and some user state that's sure to change
    after a password reset to produce a token that invalidated when it's
    used:
    1. The password field will change upon a password reset (even if the
       same password is chosen, due to password salting).
    2. The last_login field will usually be updated very shortly after
       a password reset.
    3. The email field will change if the user changes their email address.  # ADD THIS LINE
    Failing those things, settings.PASSWORD_RESET_TIMEOUT eventually
    invalidates the token.

    Running this data through salted_hmac() prevents password cracking
    attempts using the reset token, provided the secret isn't compromised.
    """
    # Truncate microseconds so that tokens are consistent even if the
    # database doesn't support microseconds.
    login_timestamp = '' if user.last_login is None else user.last_login.replace(microsecond=0, tzinfo=None)
    email_field_name = user.get_email_field_name()              # ADD THIS LINE
    email = getattr(user, email_field_name, '') or ''           # ADD THIS LINE
    return str(user.pk) + user.password + str(login_timestamp) + email + str(timestamp)  # MODIFY THIS LINE (add + email)
```

### Step 2: Add Test Case

**File:** `tests/auth_tests/test_tokens.py`

**Add new test method `test_token_invalidated_after_email_change` to the `TokenGeneratorTest` class:**

```python
def test_token_invalidated_after_email_change(self):
    """
    The token is invalidated after the user changes their email address.
    """
    user = User.objects.create_user('testuser', 'test@example.com', 'testpw')
    p0 = PasswordResetTokenGenerator()
    token = p0.make_token(user)
    # Token should be valid
    self.assertIs(p0.check_token(user, token), True)
    # Change the user's email address
    user.email = 'newemail@example.com'
    user.save()
    # Token should now be invalid
    self.assertIs(p0.check_token(user, token), False)
```

## Why This Solution Works

### Before the Fix
`hash_value = pk + password + last_login + timestamp`
- Email is NOT included
- Changing email doesn't change the hash
- Token remains valid after email change

### After the Fix
`hash_value = pk + password + last_login + email + timestamp`
- Email IS included
- Changing email changes the hash
- Token is invalidated when email changes

## Edge Cases Handled

1. **Users without email field:** Uses `getattr(user, email_field_name, '') or ''` to safely get the email, defaulting to empty string if not present

2. **Custom user models:** Uses `user.get_email_field_name()` (introduced in Django 3.1) which returns the correct field name for custom user models that override the email field

3. **Backward compatibility:** Existing tokens will be invalidated because their hash no longer matches. This is acceptable because:
   - Password reset tokens are already time-limited
   - New tokens generated with this fix will be secure
   - Users will simply request a new token if needed

## Testing

### Run the tests:
```bash
python tests/runtests.py auth_tests.test_tokens
```

### Expected output:
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

The test suite should include:
1. `test_make_token` - Basic token generation and validation
2. `test_10265` - Token consistency for users created in same request
3. `test_timeout` - Token expiration based on PASSWORD_RESET_TIMEOUT
4. `test_check_token_with_nonexistent_token_and_user` - Validation with None inputs
5. `test_token_with_different_secret` - Secret validation
6. `test_token_default_hashing_algorithm` - Hash algorithm selection
7. `test_legacy_token_validation` - Backward compatibility with old SHA1 tokens
8. `test_token_invalidated_after_email_change` - **NEW** Email change invalidates token

## Security Implications

✅ **Positive:**
- Prevents password reset token reuse after email change
- Closes a potential account takeover vector

⚠️ **Note:**
- This does not protect against email spoofing/account enumeration
- The fix assumes the email field itself is trusted (as it should be)

## Reference
Django Commit: `7f9e4524d6b23424cf44fbe1bf1f4e70f6bb066e`
Issue: Changing user's email could invalidate password reset tokens
