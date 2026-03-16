# Django Issue Fix: sqlmigrate wraps output in BEGIN/COMMIT even if database doesn't support transactional DDL

## Problem Statement

The `sqlmigrate` command was wrapping its output in `BEGIN`/`COMMIT` statements based only on whether a migration was atomic (`migration.atomic`), without considering whether the database actually supports transactional DDL.

The migration executor already handles this correctly by checking both conditions:
- `migration.atomic` - whether the migration is marked as atomic
- `connection.features.can_rollback_ddl` - whether the database supports transactional DDL

However, `sqlmigrate` only checked `migration.atomic`, leading to incorrect wrapping in databases that don't support transactional DDL (e.g., MySQL with MyISAM tables).

## Solution

### Change 1: Fix `django/core/management/commands/sqlmigrate.py` (Line 59-60)

**Before:**
```python
# Show begin/end around output only for atomic migrations
self.output_transaction = migration.atomic
```

**After:**
```python
# Show begin/end around output only for atomic migrations, and only if
# the database supports transactional DDL.
self.output_transaction = migration.atomic and connection.features.can_rollback_ddl
```

This change ensures that transaction wrappers are only added when:
1. The migration is atomic (`migration.atomic == True`)
2. **AND** the database supports transactional DDL (`connection.features.can_rollback_ddl == True`)

This matches the behavior of the migration executor's schema editor, which uses the same logic for `atomic_migration`.

### Change 2: Add Test in `tests/migrations/test_commands.py`

Added a new test `test_sqlmigrate_for_atomic_migration_without_rollback_ddl` that verifies the fix:

```python
@override_settings(MIGRATION_MODULES={"migrations": "migrations.test_migrations"})
def test_sqlmigrate_for_atomic_migration_without_rollback_ddl(self):
    """
    Transaction wrappers aren't shown for atomic migrations when the database
    doesn't support transactional DDL.
    """
    out = io.StringIO()
    with mock.patch.object(connection.features, 'can_rollback_ddl', False):
        call_command("sqlmigrate", "migrations", "0001", stdout=out)
    output = out.getvalue().lower()
    queries = [q.strip() for q in output.splitlines()]
    if connection.ops.start_transaction_sql():
        self.assertNotIn(connection.ops.start_transaction_sql().lower(), queries)
    self.assertNotIn(connection.ops.end_transaction_sql().lower(), queries)
```

This test:
- Uses an atomic migration (`migrations.test_migrations.0001_initial`)
- Mocks the database feature `can_rollback_ddl` to return `False`
- Verifies that BEGIN/COMMIT statements are NOT in the output
- Follows the same pattern as the existing `test_sqlmigrate_for_non_atomic_migration` test

## Test Results

All tests pass:
- ✅ `test_sqlmigrate_forwards` - Existing test for atomic migrations (with DDL support)
- ✅ `test_sqlmigrate_backwards` - Existing test for reverse migrations
- ✅ `test_sqlmigrate_for_non_atomic_migration` - Existing test for non-atomic migrations
- ✅ `test_sqlmigrate_for_atomic_migration_without_rollback_ddl` - New test for atomic migrations without DDL support

## Impact

### Databases Affected
- **MySQL with MyISAM**: Does not support transactional DDL → will no longer wrap in BEGIN/COMMIT
- **PostgreSQL, SQLite**: Support transactional DDL → behavior unchanged
- **Oracle, MSSQL**: Support transactional DDL → behavior unchanged

### Backward Compatibility
This is a bug fix that corrects the behavior to match the migration executor. Any code relying on the previous incorrect behavior should be updated.

## Files Modified
1. `django/core/management/commands/sqlmigrate.py` - Fixed the condition for `output_transaction`
2. `tests/migrations/test_commands.py` - Added test for the fix
