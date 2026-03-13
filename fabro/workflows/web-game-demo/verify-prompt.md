Your task is to verify that the develop-web-game skill is properly set up.

1. Call the `browser_install` Playwright MCP tool to ensure the browser is installed.
2. Call the `use_skill` tool with skill_name "develop-web-game" to load the skill instructions.
3. Read the skill instructions and identify the asset file paths referenced in them.
4. Verify the following files exist and are readable:
   - `skills/develop-web-game/scripts/web_game_playwright_client.js`
   - `skills/develop-web-game/references/action_payloads.json`
   - `skills/develop-web-game/SKILL.md`
5. Read the contents of `action_payloads.json` and confirm it contains valid JSON with a "steps" array.
6. Read the first 10 lines of `web_game_playwright_client.js` and confirm it imports playwright.
7. Write a summary report to `output/skill-verification.md` with the results.

Report success or failure for each check.
