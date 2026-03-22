Filter the bugs identified by code review to the bugs worth fixing.

We have a report of analyzed bugs which you need to filter.

To do this, follow these steps precisely:

1. Use Git to retrieve a list of modified files in this branch.
2. Use a Haiku agent to view the branch's diff, and ask the agent to return a summary of the change
3. For each bug assess if it is a false positive based on the criteria below.

Input: Read from `.ai/reviews/analyzed_bugs.xml`

Filter out the false positives. Examples of false positives:

- Nits
- Something that looks like a bug but is not actually a bug
- Pedantic issues that a senior engineer wouldn't call out
- Issues that a linter, typechecker, or compiler would catch (eg. missing or incorrect imports, type errors, broken tests, formatting issues, pedantic style issues like newlines). No need to run these build steps yourself -- it is safe to assume that they will be run separately as part of CI.
- General code quality issues (eg. lack of test coverage, general security issues, poor documentation)
- Maintainability, code smells, etc.
- Changes in functionality that are likely intentional or are directly related to the broader change
- Real issues, but are not related to the changes in the branch

Ouput:

1. Write to `.ai/reviews/valid_bugs.xml` in the same XML format with the false positives filtered out.
2. Write to `.ai/reviews/false_positives.md` a summary of the false positives you filtered out and why.

Notes:

- Do not check build signal or attempt to build or typecheck the app. These will run separately, and are not relevant to your code review.
- Make a todo list first
- It is OK to keep bugs which are pre-existing, if and only if they are both A) important and B) relevant to the changes being made.
