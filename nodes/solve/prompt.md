Goal: Added model class to app_list context
Description
	 
		(last modified by Raffaele Salmaso)
	 
I need to manipulate the app_list in my custom admin view, and the easiest way to get the result is to have access to the model class (currently the dictionary is a serialized model).
In addition I would make the _build_app_dict method public, as it is used by the two views index and app_index.


## Completed stages
- **setup**: fail
  - Script: `git clone https://github.com/django/django.git . && git checkout 0456d3e42795481a186db05719300691fe2a1029 && python -m pip install -e .`
  - Stdout:
    ```
    fatal: destination path '.' already exists and is not an empty directory.
    ```
  - Stderr: (empty)

## Context
- failure_class: deterministic
- failure_signature: setup|deterministic|script failed with exit code: <n> ## stdout fatal: destination path '.' already exists and is not an empty directory.


Fix this GitHub issue in the repository. Make the minimal code change needed.

Added model class to app_list context
Description
	 
		(last modified by Raffaele Salmaso)
	 
I need to manipulate the app_list in my custom admin view, and the easiest way to get the result is to have access to the model class (currently the dictionary is a serialized model).
In addition I would make the _build_app_dict method public, as it is used by the two views index and app_index.
