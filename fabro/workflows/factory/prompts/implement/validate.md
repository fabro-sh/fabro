Read the acceptance criteria from the feature requirements doc matching $goal under files-internal/product/features/.

For each acceptance criterion (AC-NNN-XXX.N):
- Verify the implementation satisfies "When [condition], the system shall [behavior]"
- Run relevant tests
- Check that the code matches the feature blueprint's data model, API, and UI specs

Report:
- PASS or FAIL for each acceptance criterion
- Overall satisfaction score (passed / total)
- Specific gaps or failures with file and line references

Return SUCCESS only if all acceptance criteria pass.
