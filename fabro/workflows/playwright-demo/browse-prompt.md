You have Playwright MCP tools available. Do the following:

1. First, call the `browser_install` tool to ensure the browser is installed.
2. Create the screenshots directory: `mkdir -p /home/daytona/workspace/screenshots`
3. Use `browser_navigate` to go to https://news.ycombinator.com
4. Use `browser_snapshot` to capture the page content
5. Use `browser_take_screenshot` to save a screenshot to `/home/daytona/workspace/screenshots/01-hn-front-page.png`
6. Click on the first story link
7. Use `browser_take_screenshot` to save a screenshot to `/home/daytona/workspace/screenshots/02-first-story.png`
8. Use `browser_navigate_back` to go back to the front page
9. Click on the "new" link in the nav bar
10. Use `browser_take_screenshot` to save a screenshot to `/home/daytona/workspace/screenshots/03-newest.png`

After capturing screenshots, write a brief summary of what you found on Hacker News today.
