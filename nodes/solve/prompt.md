Goal: Add check to ensure max_length fits longest choice.
Description
	
There is currently no check to ensure that Field.max_length is large enough to fit the longest value in Field.choices.
This would be very helpful as often this mistake is not noticed until an attempt is made to save a record with those values that are too long.


## Completed stages
- **setup**: fail
  - Script: `git clone https://github.com/django/django.git . && git checkout fee75d2aed4e58ada6567c464cfd22e89dc65f4a && python -m pip install -e .`
  - Stdout:
    ```
    fatal: destination path '.' already exists and is not an empty directory.
    ```
  - Stderr: (empty)

## Context
- failure_class: deterministic
- failure_signature: setup|deterministic|script failed with exit code: <n> ## stdout fatal: destination path '.' already exists and is not an empty directory.


Fix this GitHub issue in the repository. Make the minimal code change needed.

Add check to ensure max_length fits longest choice.
Description
	
There is currently no check to ensure that Field.max_length is large enough to fit the longest value in Field.choices.
This would be very helpful as often this mistake is not noticed until an attempt is made to save a record with those values that are too long.
