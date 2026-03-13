You have Playwright MCP tools available running in headed mode — a user is watching via VNC.

1. Call the `browser_install` tool to ensure the browser is installed.
2. Use `browser_navigate` to go to https://news.ycombinator.com
3. Use `browser_snapshot` to capture the page content
4. Use `browser_take_screenshot` to save a screenshot to `/root/output/01-hn-front-page.png`
5. Click on the first story link
6. Wait a moment, then use `browser_take_screenshot` to save to `/root/output/02-first-story.png`
7. Use `browser_navigate_back` to go back
8. Click on the "new" link in the nav bar
9. Use `browser_take_screenshot` to save to `/root/output/03-newest.png`
10. Navigate to https://en.wikipedia.org/wiki/Golden_Gate_Bridge
11. Use `browser_take_screenshot` to save to `/root/output/04-golden-gate.png`
12. Scroll down to see images, then take another screenshot to `/root/output/05-golden-gate-scrolled.png`

Take your time between actions so the VNC viewer can see what's happening. Write a brief summary of what you found.
