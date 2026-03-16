# Detailed Implementation: max_length Validation for CharField Choices

## Changes to django/db/models/fields/__init__.py

### Location: CharField class, check() method (line ~955)
```python
def check(self, **kwargs):
    return [
        *super().check(**kwargs),
        *self._check_max_length_attribute(**kwargs),
        *self._check_choices_fit_max_length(**kwargs),  # NEW LINE ADDED
    ]
```

### New Method: _check_choices_fit_max_length()
Added after the existing `_check_max_length_attribute()` method (starting around line 982):

```python
def _check_choices_fit_max_length(self, **kwargs):
    if not self.choices or self.max_length is None:
        return []

    def get_choice_values(choices):
        """Extract all choice values from choices list (handles grouped choices)."""
        for choice in choices:
            try:
                choice_value, choice_display = choice
            except (TypeError, ValueError):
                # Skip if not a proper pair
                continue
            
            # Check if this is a group (second element is iterable but not string)
            if isinstance(choice_display, (list, tuple)):
                # It's a grouped choice, recurse
                yield from get_choice_values(choice_display)
            else:
                # It's a regular choice
                yield choice_value

    errors = []
    for choice_value in get_choice_values(self.choices):
        # Convert to string to get the length (choice values are typically strings)
        choice_str = str(choice_value) if choice_value is not None else ''
        if len(choice_str) > self.max_length:
            errors.append(
                checks.Error(
                    "Field max_length is not large enough to fit the longest "
                    "choice value '{value}' (length {length}). "
                    "Increase max_length to at least {length}.".format(
                        value=choice_str,
                        length=len(choice_str),
                    ),
                    obj=self,
                    id='fields.E122',
                )
            )
            # Only report the first error to avoid too many messages
            break

    return errors
```

## Changes to tests/check_framework/test_model_checks.py

### New Test Class: CharFieldChoicesTests
Added at the end of the file (after line 360):

```python
@isolate_apps('check_framework', attr_name='apps')
@override_system_checks([checks.model_checks.check_all_models])
class CharFieldChoicesTests(SimpleTestCase):
    def test_charfield_choices_with_max_length_too_short(self):
        """CharField max_length must be large enough for all choice values."""
        class Model(models.Model):
            status = models.CharField(
                max_length=2,
                choices=[
                    ('active', 'Active'),
                    ('inactive', 'Inactive'),  # 'inactive' is 8 chars
                ]
            )

        errors = checks.run_checks(app_configs=self.apps.get_app_configs())
        self.assertEqual(len(errors), 1)
        self.assertEqual(errors[0].id, 'fields.E122')
        self.assertIn('max_length', errors[0].msg.lower())

    def test_charfield_choices_with_sufficient_max_length(self):
        """CharField max_length large enough should not raise error."""
        class Model(models.Model):
            status = models.CharField(
                max_length=10,
                choices=[
                    ('active', 'Active'),
                    ('inactive', 'Inactive'),
                ]
            )

        errors = checks.run_checks(app_configs=self.apps.get_app_configs())
        # Filter out unrelated checks, only look for E122
        choice_errors = [e for e in errors if e.id == 'fields.E122']
        self.assertEqual(len(choice_errors), 0)

    def test_charfield_grouped_choices_with_max_length_too_short(self):
        """CharField max_length check should work with grouped choices."""
        class Model(models.Model):
            status = models.CharField(
                max_length=3,
                choices=[
                    ('Group1', [
                        ('a', 'Option A'),
                        ('verylongvalue', 'Very Long Value'),  # 14 chars
                    ]),
                ]
            )

        errors = checks.run_checks(app_configs=self.apps.get_app_configs())
        choice_errors = [e for e in errors if e.id == 'fields.E122']
        self.assertEqual(len(choice_errors), 1)

    def test_charfield_no_choices(self):
        """CharField without choices should not raise E122."""
        class Model(models.Model):
            status = models.CharField(max_length=10)

        errors = checks.run_checks(app_configs=self.apps.get_app_configs())
        choice_errors = [e for e in errors if e.id == 'fields.E122']
        self.assertEqual(len(choice_errors), 0)

    def test_charfield_empty_choices(self):
        """CharField with empty choices should not raise E122."""
        class Model(models.Model):
            status = models.CharField(max_length=10, choices=[])

        errors = checks.run_checks(app_configs=self.apps.get_app_configs())
        choice_errors = [e for e in errors if e.id == 'fields.E122']
        self.assertEqual(len(choice_errors), 0)
```

## Key Design Decisions

1. **Using Django's Check Framework**: The validation is integrated into Django's system checks, which runs during `manage.py check` and at startup. This provides early detection without runtime overhead.

2. **Error ID `fields.E122`**: A new, unused error ID was chosen to avoid conflicts with existing Django errors.

3. **Handling Grouped Choices**: The `get_choice_values()` generator function recursively processes grouped choices, enabling support for nested choice structures.

4. **Reporting Only First Error**: To avoid overwhelming users with multiple similar errors, only the first problematic choice is reported.

5. **String Conversion**: Choice values are converted to strings before checking length, handling various value types (int, char, etc.) consistently.

6. **Early Exit Conditions**: The method returns early if there are no choices or if max_length is None, avoiding unnecessary processing.

## Testing Coverage

| Test Case | Scenario | Expected Result |
|-----------|----------|-----------------|
| `test_charfield_choices_with_max_length_too_short` | max_length=2 with 'inactive' (8 chars) | Error E122 raised |
| `test_charfield_choices_with_sufficient_max_length` | max_length=10 with 'inactive' (8 chars) | No error |
| `test_charfield_grouped_choices_with_max_length_too_short` | Nested choices with long value | Error E122 raised |
| `test_charfield_no_choices` | CharField without choices | No error |
| `test_charfield_empty_choices` | CharField with empty choices list | No error |

All tests verify that the validation correctly identifies problematic configurations while avoiding false positives.
