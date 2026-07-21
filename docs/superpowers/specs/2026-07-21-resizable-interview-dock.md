# Resizable Interview Dock with Background Scroll

## Intent Description

When workflow questions appear in the Fabro UI, the interview dock panel renders at the bottom of the screen as a fixed-position overlay. This panel currently has a hardcoded height (`18rem` clearance when questions are present, `5rem` when showing the steer bar), and content behind the panel cannot be scrolled while the panel is visible. Users working with long stage lists, extensive file change lists, or deep content in the Overview tab must answer or dismiss questions before they can navigate the obscured interface regions.

This change adds resize and scroll capabilities to the interview dock panel:

1. **Vertical resize**: Users can drag a handle at the top edge of the interview dock to adjust its height, similar to resizing a terminal pane or chat sidebar. The panel remembers the user's preferred height across sessions.
2. **Background scroll**: Content behind the interview dock remains scrollable. Users can scroll through stages, files, or overview content without needing to resize or dismiss the interview panel first.

These improvements allow users to reference obscured information while formulating answers, compare question context with earlier stages, or review file changes without losing the current interview state.

## Architecture Specification

### Component Changes

#### Interview Dock Resize Mechanism

- Add a drag handle to the top edge of `InterviewDock` (`apps/fabro-web/app/components/interview-dock.tsx`). The handle appears as a horizontal bar with a visual affordance (e.g., centered grip icon or subtle hover highlight).
- Implement vertical resize via mouse/touch drag interaction. During drag, update the dock height in real time. Use the same interaction pattern as `AskFabroSidebar` resize (`apps/fabro-web/app/components/chats/ask-fabro-sidebar.tsx` lines 84–102), which manages width resize with a handle, `onResizeActiveChange`, and pixel delta computation.
- Constrain dock height to a minimum (e.g., `12rem` to ensure question text, context, and controls remain visible) and maximum (e.g., `80vh` to keep some background content visible).
- Persist the user's chosen height in `localStorage` under a key like `fabro.interviewDock.height`. On mount, read the stored height and apply it; fall back to the current default (`18rem`) if no preference exists.
- Expose the current dock height and resize-active state from `InterviewDock` via callback props. The parent (`RunDetailDockedControls`) passes these values up to `RunDetail`.

#### CSS Variable Update

- Change `--fabro-interview-dock-clearance` in `apps/fabro-web/app/routes/run-detail.tsx` (line 303) from a binary `18rem` / `5rem` value to the user's actual dock height. When no questions are pending and the steer bar is visible, keep the existing `5rem` clearance.
- When resize is active, suppress the CSS transition on `--fabro-interview-dock-clearance` (similar to how `--fabro-ask-sidebar-transition` is set to `none` during sidebar resize; see `apps/fabro-web/app/routes/run-detail/docked-controls.tsx` lines 55–60). This prevents janky animation during drag.

#### Background Scroll Behavior

The interview dock is currently `position: fixed` with `bottom: 0` (`apps/fabro-web/app/routes/run-detail/docked-controls.tsx` line 148). Content below the dock is obscured but not scrollable past the dock's top edge.

To enable scroll behind the dock:

- Keep the dock as `position: fixed; bottom: 0` so it remains anchored during scroll.
- Content panes (Overview, Stages, Files Changed, etc.) already use `pb-[var(--fabro-interview-dock-clearance)]` or similar padding to reserve space below their scroll containers. This padding ensures scrollable content isn't hidden behind the dock.
- Verify that all scrollable regions (`run-overview.tsx` lines 145 & 149, `run-stages.tsx` line 1506, `run-files/virtualized-diff-list.tsx` line 20, etc.) continue to apply the clearance padding. No changes are required if the existing padding is already correct—users will be able to scroll through the full content height because the padding accounts for the dock's presence.
- The dock itself should not interfere with pointer events on the background content. Since the dock is a distinct fixed layer and the background is scrollable within its own container, this should already work without changes.

### Files Modified

- `apps/fabro-web/app/components/interview-dock.tsx`: Add resize handle, drag interaction, height state, localStorage persistence, and callbacks to expose height/resize-active state.
- `apps/fabro-web/app/routes/run-detail/docked-controls.tsx`: Accept dock height and resize-active state from `InterviewDock`, pass them up to `RunDetail`.
- `apps/fabro-web/app/routes/run-detail.tsx`: Receive dock height from `RunDetailDockedControls`, set `--fabro-interview-dock-clearance` to the dynamic height, suppress transition during resize.

### Constraints

- Minimum dock height: `12rem` (enough to show question, context snippet, and answer controls).
- Maximum dock height: `80vh` (ensures at least 20% of the viewport remains visible above the dock).
- The default height (when no localStorage preference exists) remains `18rem` to match current behavior.
- Resize handle should be visually distinct but not intrusive—consistent with Fabro's existing drag handle patterns (e.g., `AskFabroSidebar` resize handle).
- Scrollable content panes must not clip or hide content behind the dock. Existing `pb-[var(--fabro-interview-dock-clearance)]` padding ensures this; verify no regressions.

## Acceptance Criteria

### Resize Functionality

1. When an interview question is pending, a resize handle appears at the top edge of the interview dock panel.
2. Dragging the resize handle upward increases dock height; dragging downward decreases it.
3. Dock height is constrained to `[12rem, 80vh]`. Attempts to drag beyond these bounds clamp to the limit.
4. During drag, the dock height updates smoothly in real time without layout jank.
5. Releasing the drag handle persists the new height to `localStorage`.
6. Refreshing the page or navigating away and returning to a run detail page with pending questions restores the user's last chosen dock height.
7. If no height preference exists in `localStorage`, the dock defaults to `18rem`.

### Scroll Functionality

8. When the interview dock is visible, scrollable content regions (Overview stage cards, Stages sidebar, Files Changed list) remain scrollable.
9. Scrolling a content pane to the bottom reveals all content; the `pb-[var(--fabro-interview-dock-clearance)]` padding ensures the last item is not obscured by the dock.
10. Scrolling behavior is consistent across all run detail tabs (Overview, Stages, Files Changed, Events, etc.).

### Visual and Interaction Polish

11. The resize handle has a hover state indicating it is draggable.
12. The cursor changes to `ns-resize` (vertical resize cursor) when hovering over the handle.
13. The `--fabro-interview-dock-clearance` CSS transition is suppressed (set to `none`) while resize is active, then restored when the drag ends.
14. The dock's appearance (colors, spacing, borders) remains unchanged aside from the addition of the resize handle.

### Edge Cases

15. If the viewport is resized such that `80vh` becomes smaller than `12rem`, the dock height clamps to `12rem` (minimum takes precedence).
16. If the user's stored height exceeds the new maximum after a viewport resize, the dock height clamps to `80vh` on next mount.
17. When no questions are pending and the steer bar is visible, the existing `5rem` clearance is used (resize handle is not shown).
18. Switching between runs or navigating between tabs does not reset the dock height unless the stored preference is explicitly cleared.

## Ambiguity Log

| Decision | Classification | Resolved By | Rationale / Answer |
|----------|---------------|-------------|-------------------|
| Minimum dock height value | inferable | codebase & design | `12rem` is chosen to match the approximate height needed to show a question header, a few lines of question text, a context snippet, and answer controls (buttons or a single-line textarea). This is consistent with the existing interview dock's visual structure (see `apps/fabro-web/app/components/interview-dock.tsx` lines 100–122). |
| Maximum dock height constraint | inferable | design patterns | `80vh` ensures at least 20% of the viewport remains visible above the dock. This is a common UX pattern for resizable bottom panels (e.g., VS Code terminal, Chrome DevTools) and prevents the dock from fully obscuring the UI. The Ask Fabro sidebar uses a similar pattern with a maximum width (`SIDEBAR_MAX_WIDTH` is `60vw` in prototype code, though the current implementation uses a fixed max). |
| Default height for first-time users | inferable | existing behavior | The current clearance is `18rem` when questions are present (line 303 of `run-detail.tsx`). Preserving this as the default maintains continuity and avoids surprising existing users. |
| Height persistence mechanism | inferable | existing patterns | The codebase uses `localStorage` for client-side preferences (e.g., `AskFabroSidebar` width persistence pattern from the chats prototype, see `docs/superpowers/specs/2026-05-16-chats-new-prototype-design.md` lines 145–147). This is the standard approach for per-user UI state that doesn't require server-side storage. |
| Resize handle visual design | inferable | existing patterns | The Ask Fabro sidebar resize handle pattern (vertical bar, hover state, cursor change) is the established precedent. The interview dock resize handle should follow the same visual language: a thin horizontal bar with a centered grip affordance, `ns-resize` cursor on hover, and subtle hover highlight. |
| Behavior when stored height is invalid | inferable | defensive coding | If `localStorage` returns a non-numeric or out-of-bounds value, fall back to the default `18rem`. This is standard defensive handling for user-supplied preferences. |
| Transition suppression during resize | inferable | existing patterns | The `--fabro-ask-sidebar-transition` variable is set to `none` during resize and restored afterward to prevent janky transitions (see `apps/fabro-web/app/routes/run-detail/docked-controls.tsx` lines 57–59). The same pattern applies to `--fabro-interview-dock-clearance` transitions to avoid visual stutter during drag. |
| Scrollable content padding verification | inferable | existing code | All scrollable panes already use `pb-[var(--fabro-interview-dock-clearance)]` or a calculated variant (e.g., `pb-[calc(1.5rem+var(--fabro-interview-dock-clearance,0px))]`). See `apps/fabro-web/app/routes/run-overview.tsx` lines 145 & 149, `run-stages.tsx` line 1506, `run-files/virtualized-diff-list.tsx` line 20. These ensure content is not clipped by the fixed dock. The feature works correctly if the clearance variable is updated to reflect the dynamic height. |
| Resize interaction pattern | inferable | existing patterns | The `AskFabroSidebar` resize implementation (pointer events, drag delta computation, state updates) is the reference. The interview dock resize uses the same approach: `onPointerDown` on the handle, `onPointerMove` on a global listener during drag, `onPointerUp` to end drag, and state updates to propagate height changes. |
| Z-index and layering | inferable | existing code | The interview dock is already `z-30` (`apps/fabro-web/app/routes/run-detail/docked-controls.tsx` line 149). Scrollable content is not `position: fixed` and thus layers below the dock. No z-index changes are needed. |
| Mobile/touch support | inferable | existing patterns | The Ask Fabro sidebar resize uses pointer events (`onPointerDown`, `onPointerMove`, `onPointerUp`), which handle both mouse and touch. The interview dock resize should use the same pointer event API for consistent touch support. |
| Documented inferences approved | requires-stakeholder-input | human | "ok" — Human accepted all documented inferences without changes. |

**All findings resolved.**

## Cross-Artifact Consistency Gate

- [x] Intent is unambiguous — two developers would interpret it the same way.
- [x] Every behavior/goal in the intent maps to at least one acceptance criterion.
- [x] Architecture constrains implementation without over-engineering.
- [x] Same concepts named consistently across all three artifacts.
- [x] No artifact contradicts another.
- [x] Every gap/ambiguity finding is logged — inferable with rationale, or resolved by the human.
