Provide a code review for this branch relative to the base branch for bugs and defects.

To do this, follow these steps precisely:

1. Use Git to retrieve a list of modified files in this branch.
2. Use a Haiku agent to give you a list of file paths to (but not the contents of) any relevant CLAUDE.md files from the codebase: the root CLAUDE.md file (if one exists), as well as any CLAUDE.md files in the directories whose files the pull request modified
3. Use a Haiku agent to view the branch's diff, and ask the agent to return a summary of the change
4. Then, launch 5 parallel Opus agents to independently code review the change for production bugs and vulnerabilities.
   a. Agent #1: Read the git blame and history of the code modified, to identify any bugs in light of that historical context
   b. Agent #2: Read code comments in the modified files, and make sure the changes in the pull request comply with any guidance in the comments.
   c. Agent #3-5: Read the file changes in this branch, then do a scan for potential bugs. Focus on bugs with production / end-user impact, and avoid small issues and nitpicks.

Output a report with all the bugs using this format:

<code_review>
   <bug>
      <title>title of bug</title>
      <description>brief description of bug</description>
      <location>
         <file>lib/crates/fabro-cli/src/commands/resume.rs</file>
         <start_line>115</start_line>
         <end_line>115</end_line>
      </location>
      <severity>critical/high/medium/low</severity>
   </bug>
   <bug>...</bug>
</code_review>

Write the report to: `.ai/tmp/candidate_bugs.xml`

Notes:

- Do not check build signal or attempt to build or typecheck the app. These will run separately, and are not relevant to your code review.
- Include all potential bugs of all severity (critical/high/medium/low) that have production / end-user impact. (We will analyze them separately later.)
- Make a todo list first