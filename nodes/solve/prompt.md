Goal: Allow ValidationErrors to equal each other when created identically
Description
	 
		(last modified by kamni)
	 
Currently ValidationErrors (django.core.exceptions.ValidationError) that have identical messages don't equal each other, which is counter-intuitive, and can make certain kinds of testing more complicated. Please add an __eq__ method that allows two ValidationErrors to be compared. 
Ideally, this would be more than just a simple self.messages == other.messages. It would be most helpful if the comparison were independent of the order in which errors were raised in a field or in non_field_errors.



## Additional Context

I probably wouldn't want to limit the comparison to an error's message but rather to its full set of attributes (message, code, params). While params is always pushed into message when iterating over the errors in an ValidationError, I believe it can be beneficial to know if the params that were put inside are the same.
​PR

## Completed stages
- **setup**: fail
  - Script: `git clone https://github.com/django/django.git . && git checkout 16218c20606d8cd89c5393970c83da04598a3e04 && python -m pip install -e .`
  - Stdout:
    ```
    fatal: destination path '.' already exists and is not an empty directory.
    ```
  - Stderr: (empty)

## Context
- failure_class: deterministic
- failure_signature: setup|deterministic|script failed with exit code: <n> ## stdout fatal: destination path '.' already exists and is not an empty directory.


Fix this GitHub issue in the repository. Make the minimal code change needed.

Allow ValidationErrors to equal each other when created identically
Description
	 
		(last modified by kamni)
	 
Currently ValidationErrors (django.core.exceptions.ValidationError) that have identical messages don't equal each other, which is counter-intuitive, and can make certain kinds of testing more complicated. Please add an __eq__ method that allows two ValidationErrors to be compared. 
Ideally, this would be more than just a simple self.messages == other.messages. It would be most helpful if the comparison were independent of the order in which errors were raised in a field or in non_field_errors.



## Additional Context

I probably wouldn't want to limit the comparison to an error's message but rather to its full set of attributes (message, code, params). While params is always pushed into message when iterating over the errors in an ValidationError, I believe it can be beneficial to know if the params that were put inside are the same.
​PR