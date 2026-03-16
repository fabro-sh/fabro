All 8 tests pass. Here's a summary of the changes made:

### Changes

1. **`django/conf/global_settings.py`**: Changed `FILE_UPLOAD_PERMISSIONS` default from `None` to `0o644`.

2. **`docs/ref/settings.txt`**: 
   - Updated the documented default from `None` to `0o644`.
   - Changed "If this isn't given or is `None`" to "If this is set to `None`" since it now has a non-None default.
   - Added a `versionchanged:: 3.0` note indicating the old default was `None`.

3. **`docs/howto/deployment/checklist.txt`**: Updated the deployment checklist to reflect that the inconsistent permissions issue only occurs when the setting is explicitly set to `None` (no longer the default).

4. **`tests/test_utils/tests.py`**: Updated the assertion from `assertIsNone` to `assertEqual(..., 0o644)` for the default `file_permissions_mode`.

5. **`tests/file_storage/tests.py`**: Updated `test_file_upload_default_permissions` to expect `0o644` (instead of umask-dependent `0o666 & ~umask`) and removed the `@override_settings(FILE_UPLOAD_PERMISSIONS=None)` decorator so it uses the new default.

6. **`tests/staticfiles_tests/test_storage.py`**: Updated `test_collect_static_files_default_permissions` to expect `0o644` for files and removed the `FILE_UPLOAD_PERMISSIONS=None` override.