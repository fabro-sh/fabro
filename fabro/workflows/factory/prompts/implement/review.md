Review the implementation with fresh eyes. You did not write this code.

Read the feature blueprint matching $goal under files-internal/architecture/features/.
Read the implementation files created or modified by the previous step.

Evaluate the code for:
- **Correctness:** Logic errors, off-by-one mistakes, unhandled edge cases
- **Security:** Injection risks, unsafe input handling, exposed secrets
- **Consistency:** Does it follow the patterns in the foundation blueprints and surrounding codebase?
- **Duplication:** Unnecessary copy-paste that should be extracted
- **Simplicity:** Overly complex code that could be simpler without losing clarity

Do not rewrite the code. Report specific issues with file and line references.

If no significant issues are found, return SUCCESS.
If issues are found, return FAIL with a numbered list of issues to fix.
