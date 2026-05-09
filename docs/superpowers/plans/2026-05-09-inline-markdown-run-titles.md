# Inline Markdown Run Titles

## Summary

Render run titles with a small allowlist of inline Markdown decoration in the web UI: backticks become `<code>`, `**bold**` becomes `<strong>`, and `_italic_` / `*italic*` becomes `<em>`. Do not render Markdown block structures, raw HTML, images, or clickable links in titles. Keep the API/server data model unchanged.

## Key Changes

- Add `apps/fabro-web/app/components/inline-markdown.tsx` exporting `InlineMarkdown({ content, className })`.
- Use `marked`'s `Lexer.lexInline(...)` to tokenize inline Markdown, then map tokens to React elements manually. Do not use `dangerouslySetInnerHTML`.
- Supported rendered elements:
  - `codespan` -> `<code>` with compact existing-theme styling.
  - `strong` -> `<strong>`.
  - `em` -> `<em>`.
  - plain/escaped text -> React text.
- Unsupported inline syntax renders as safe text:
  - links render their label only, no `<a>`.
  - images render alt text only, no `<img>`.
  - raw HTML displays literally as text.
  - hard breaks/newlines render as normal title whitespace.
- Replace visible run-title render sites with `InlineMarkdown`:
  - run detail breadcrumb, heading, and delete confirmation in `apps/fabro-web/app/routes/run-detail.tsx`.
  - run list row title in `apps/fabro-web/app/routes/runs.tsx`.
  - workflow run list title in `apps/fabro-web/app/routes/workflow-runs.tsx`.
- Leave non-visual/plain-text title uses unchanged:
  - document metadata title.
  - filtering/search.
  - API/client/server fields.

## Tests

- Add `apps/fabro-web/app/components/inline-markdown.test.tsx`.
- Cover these cases:
  - ``Move from `[server.integrations.github]` to `[run.integrations.github]` `` renders two `<code>` elements without visible backticks.
  - `**bold**` and `_italic_` render `<strong>` and `<em>`.
  - `# heading`, lists, and blockquotes remain text and do not create block nodes.
  - `[label](javascript:alert(1))`, `![alt](x)`, and raw HTML do not create `a`, `img`, or HTML nodes.
- Add a small `run-detail.test.ts` assertion that a Markdown-formatted run title produces inline `<code>` in the visible detail heading.
- Verify with:
  - `cd apps/fabro-web && bun test`
  - `cd apps/fabro-web && bun run typecheck`

## Assumptions

- This is frontend-only; no OpenAPI, Rust server, or stored title migration is needed.
- Links should not be clickable inside run titles because titles already appear inside links and navigation surfaces.
- Only `code`, `strong`, and `em` are intentionally styled for v1; other Markdown stays textual.
