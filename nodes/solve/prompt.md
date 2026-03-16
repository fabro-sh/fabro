Goal: delete() on instances of models without any dependencies doesn't clear PKs.
Description
	
Deleting any model with no dependencies not updates the PK on the model. It should be set to None after .delete() call.
See Django.db.models.deletion:276-281. Should update the model line 280.



## Additional Context

Reproduced at 1ffddfc233e2d5139cc6ec31a4ec6ef70b10f87f. Regression in bc7dd8490b882b2cefdc7faf431dc64c532b79c9. Thanks for the report.
Regression test.
I have attached a simple fix which mimics what ​https://github.com/django/django/blob/master/django/db/models/deletion.py#L324-L326 does for multiple objects. I am not sure if we need ​https://github.com/django/django/blob/master/django/db/models/deletion.py#L320-L323 (the block above) because I think field_updates is only ever filled if the objects are not fast-deletable -- ie ​https://github.com/django/django/blob/master/django/db/models/deletion.py#L224 is not called due to the can_fast_delete check at the beginning of the collect function. That said, if we want to be extra "safe" we can just move lines 320 - 326 into an extra function and call that from the old and new location (though I do not think it is needed).

## Completed stages
- **setup**: fail
  - Script: `git clone https://github.com/django/django.git . && git checkout 19fc6376ce67d01ca37a91ef2f55ef769f50513a && python -m pip install -e .`
  - Stdout:
    ```
    fatal: destination path '.' already exists and is not an empty directory.
    ```
  - Stderr: (empty)

## Context
- failure_class: deterministic
- failure_signature: setup|deterministic|script failed with exit code: <n> ## stdout fatal: destination path '.' already exists and is not an empty directory.


Fix this GitHub issue in the repository. Make the minimal code change needed.