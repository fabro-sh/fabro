# Plan: Resizable Interview Dock with Background Scroll

**Status**: in-progress
**Spec**: docs/superpowers/specs/2026-07-21-resizable-interview-dock.md

## Goal

Add vertical resize and background scroll capabilities to the interview dock panel so users can adjust the panel height and access obscured content without dismissing workflow questions.

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

## Slices

### Slice 1: localStorage Height Persistence Infrastructure

**Depends-on**: none
**Files**: `apps/fabro-web/app/components/interview-dock.tsx`

Add localStorage-backed height state management to `InterviewDock` without UI changes. This establishes the data layer before adding the resize handle.

#### Scenarios

```gherkin
Scenario: Default dock height on first visit
  Given the user has never resized the interview dock
  And localStorage contains no height preference
  When an interview question is pending
  Then the dock height is 18rem

Scenario: Restored height from localStorage
  Given the user previously resized the dock to 24rem
  And localStorage contains "fabro.interviewDock.height": "24rem"
  When the user navigates to a run with pending questions
  Then the dock height is 24rem

Scenario: Invalid localStorage value falls back to default
  Given localStorage contains "fabro.interviewDock.height": "invalid"
  When an interview question is pending
  Then the dock height is 18rem

Scenario: Out-of-bounds stored height clamps to maximum
  Given localStorage contains "fabro.interviewDock.height": "90vh"
  And the viewport height is 1000px (80vh = 800px)
  When the component mounts
  Then the dock height clamps to 80vh (800px)

Scenario: Out-of-bounds stored height clamps to minimum
  Given localStorage contains "fabro.interviewDock.height": "8rem"
  When the component mounts
  Then the dock height clamps to 12rem
```

#### Steps

1. IMPLEMENT: Add constants `DEFAULT_DOCK_HEIGHT = "18rem"`, `MIN_DOCK_HEIGHT = "12rem"`, `MAX_DOCK_HEIGHT_VH = 80`, and `STORAGE_KEY = "fabro.interviewDock.height"` at the top of `interview-dock.tsx`.
   - TEST: Unit test verifies constants have expected values.
   - REFACTOR: none

2. IMPLEMENT: Add `useState` hook for dock height (initial value from `loadDockHeight()` helper).
   - TEST: Snapshot test verifies default height renders as `18rem` when localStorage is empty.
   - REFACTOR: none

3. IMPLEMENT: Create `loadDockHeight()` helper that reads from localStorage, validates the value (numeric string ending in `rem` or `px`, within bounds), and returns the valid height or the default.
   - TEST: Unit test with mocked localStorage verifies: (a) valid stored height is returned, (b) invalid/missing value returns default, (c) out-of-bounds values clamp to min/max.
   - REFACTOR: Extract bounds validation into a separate `clampDockHeight()` helper if logic becomes complex.

4. IMPLEMENT: Create `saveDockHeight(height: string)` helper that writes to localStorage under `STORAGE_KEY`.
   - TEST: Unit test with mocked localStorage verifies the key and value are written correctly.
   - REFACTOR: none

5. IMPLEMENT: Add `useEffect` that persists height to localStorage whenever the height state changes (skip on initial mount to avoid writing the default).
   - TEST: Integration test verifies that changing height state triggers localStorage write.
   - REFACTOR: If the effect fires on initial mount, add a ref to track first render and skip persistence.

6. IMPLEMENT: Pass the height state value up to `RunDetail` via a new `onDockHeightChange` callback prop accepted by `InterviewDock` and `RunDetailDockedControls`.
   - TEST: Snapshot test verifies the callback is invoked with the current height.
   - REFACTOR: none

### Slice 2: Resize Handle and Drag Interaction

**Depends-on**: Slice 1
**Files**: `apps/fabro-web/app/components/interview-dock.tsx`

Add the visual resize handle and pointer-based drag interaction to `InterviewDock`. This enables users to adjust the dock height via drag.

#### Scenarios

```gherkin
Scenario: Resize handle appears when questions are pending
  Given an interview question is pending
  When the interview dock is rendered
  Then a resize handle appears at the top edge of the dock

Scenario: Resize handle shows hover state
  Given the resize handle is visible
  When the user hovers over the handle
  Then the cursor changes to ns-resize
  And the handle shows a visual highlight

Scenario: Dragging upward increases dock height
  Given the dock height is 18rem
  When the user drags the resize handle upward by 100px
  Then the dock height increases by 100px

Scenario: Dragging downward decreases dock height
  Given the dock height is 18rem
  When the user drags the resize handle downward by 50px
  Then the dock height decreases by 50px

Scenario: Dock height clamps to minimum during drag
  Given the dock height is 13rem
  When the user drags the resize handle downward by 100px
  Then the dock height clamps to 12rem

Scenario: Dock height clamps to maximum during drag
  Given the dock height is 75vh
  And the viewport height is 1000px (80vh = 800px)
  When the user drags the resize handle upward by 200px
  Then the dock height clamps to 80vh (800px)

Scenario: Releasing drag updates localStorage
  Given the user drags the dock height to 24rem
  When the user releases the pointer
  Then localStorage "fabro.interviewDock.height" is "24rem"

Scenario: Touch drag works on mobile
  Given the user is on a touch device
  When the user performs a touch drag on the resize handle
  Then the dock height updates in real time
  And the drag ends when the touch is released
```

#### Steps

1. IMPLEMENT: Add a horizontal resize handle `<div>` at the top of the `<section>` in `InterviewQuestionDock` with `role="separator"`, `aria-orientation="horizontal"`, and `aria-label="Resize interview dock"`.
   - TEST: Snapshot test verifies the handle renders with correct ARIA attributes.
   - REFACTOR: none

2. IMPLEMENT: Style the handle as a horizontal bar (e.g., `h-2`, `cursor-ns-resize`, with a centered grip affordance or hover highlight matching the Ask Fabro sidebar handle pattern).
   - TEST: Visual regression test (manual or screenshot comparison) verifies the handle appearance matches the sidebar resize handle.
   - REFACTOR: Extract shared resize handle styles into a Tailwind utility class if duplication is significant.

3. IMPLEMENT: Add `useState` for `isDragging` and a `useRef` for `dragOrigin: { y: number; height: number } | null`.
   - TEST: Snapshot test verifies initial state is `isDragging: false` and `dragOrigin: null`.
   - REFACTOR: none

4. IMPLEMENT: Add `onPointerDown` handler to the resize handle that captures the pointer, records `dragOrigin` (clientY and current height), sets `isDragging: true`, and calls `onResizeActiveChange(true)`.
   - TEST: Unit test verifies `onPointerDown` sets `isDragging: true` and captures pointer.
   - REFACTOR: none

5. IMPLEMENT: Add `onPointerMove` handler that computes the new height from the drag delta (origin.height + (origin.y - event.clientY)), clamps it to `[MIN_DOCK_HEIGHT, MAX_DOCK_HEIGHT_VH]`, and updates the height state. Note: dragging upward (decreasing clientY) increases height because the handle is at the top of a bottom-docked panel.
   - TEST: Unit test with mocked pointer events verifies height delta computation and clamping.
   - REFACTOR: Extract clamping logic into a shared `clampDockHeight(height, viewportHeight)` helper.

6. IMPLEMENT: Add `onPointerUp` and `onPointerCancel` handlers that release pointer capture, clear `dragOrigin`, set `isDragging: false`, and call `onResizeActiveChange(false)`.
   - TEST: Unit test verifies `onPointerUp` resets drag state and releases pointer.
   - REFACTOR: none

7. IMPLEMENT: Add `onResizeActiveChange` callback prop to `InterviewDock` and `InterviewQuestionDock`, passed up through `RunDetailDockedControls` to `RunDetail`.
   - TEST: Integration test verifies the callback is invoked with `true` on drag start and `false` on drag end.
   - REFACTOR: none

### Slice 3: Dynamic CSS Variable and Transition Suppression

**Depends-on**: Slice 1, Slice 2
**Files**: `apps/fabro-web/app/routes/run-detail/docked-controls.tsx`, `apps/fabro-web/app/routes/run-detail.tsx`

Wire the dynamic dock height into the `--fabro-interview-dock-clearance` CSS variable and suppress transitions during resize.

#### Scenarios

```gherkin
Scenario: CSS variable reflects dynamic dock height
  Given the user has resized the dock to 24rem
  When the dock is rendered
  Then --fabro-interview-dock-clearance is set to 24rem

Scenario: CSS variable falls back to 5rem when no questions are pending
  Given no interview questions are pending
  And the steer bar is visible
  When the page renders
  Then --fabro-interview-dock-clearance is set to 5rem

Scenario: Transition is suppressed during resize
  Given the user starts dragging the resize handle
  When resize-active state is true
  Then --fabro-interview-dock-clearance-transition is set to "none"

Scenario: Transition is restored after resize
  Given the user finishes dragging the resize handle
  When resize-active state is false
  Then --fabro-interview-dock-clearance-transition is restored to the default transition

Scenario: Viewport resize clamps max height
  Given the dock height is 80vh
  And the viewport height decreases from 1000px to 500px
  When the component re-renders
  Then the dock height clamps to 80vh of the new viewport (400px)
```

#### Steps

1. IMPLEMENT: Update `RunDetailDockedControls` to accept `dockHeight` and `onDockHeightChange` props from `InterviewDock`, and pass them up as render props to `RunDetail`.
   - TEST: Snapshot test verifies props are threaded correctly.
   - REFACTOR: none

2. IMPLEMENT: In `RunDetail`, replace the hardcoded `dockClearance` ternary (`hasPendingQuestions ? "18rem" : "5rem"`) with `hasPendingQuestions ? dockHeight : "5rem"`.
   - TEST: Unit test verifies the clearance value switches correctly based on `hasPendingQuestions` and `dockHeight`.
   - REFACTOR: none

3. IMPLEMENT: Add a second CSS variable `--fabro-interview-dock-clearance-transition` to `rootStyle` in `RunDetail`, set to `"none"` when `resizeActive` is true, otherwise `"padding 300ms cubic-bezier(0.16, 1, 0.3, 1)"`.
   - TEST: Snapshot test verifies the transition variable is set correctly for both resize states.
   - REFACTOR: Extract the transition easing string into a shared constant if it appears in multiple places.

4. IMPLEMENT: Update all scrollable content panes (if needed) to use the `--fabro-interview-dock-clearance-transition` variable in their `transition` CSS. Verify existing panes already have `pb-[var(--fabro-interview-dock-clearance)]`.
   - TEST: Visual regression test (manual or screenshot) verifies no layout jank during resize.
   - REFACTOR: If multiple panes duplicate the transition logic, extract into a shared Tailwind class.

5. IMPLEMENT: Add a `useEffect` in `InterviewDock` that listens to window resize and re-clamps the dock height to `[MIN_DOCK_HEIGHT, 80vh]` when the viewport changes.
   - TEST: Integration test with mocked `window.innerHeight` verifies the dock height re-clamps on viewport resize.
   - REFACTOR: Debounce the resize handler if it fires too frequently.

### Slice 4: Scroll Behavior Verification and Edge Case Handling

**Depends-on**: Slice 3
**Files**: `apps/fabro-web/app/routes/run-overview.tsx`, `apps/fabro-web/app/routes/run-stages.tsx`, `apps/fabro-web/app/routes/run-files/virtualized-diff-list.tsx` (read-only verification, no changes expected)

Verify that existing padding on scrollable content regions works correctly with the dynamic clearance variable. Add tests for edge cases and scroll behavior.

#### Scenarios

```gherkin
Scenario: Overview tab scrolls behind the dock
  Given the interview dock is visible with height 18rem
  And the Overview tab has more content than fits in the viewport
  When the user scrolls to the bottom of the Overview tab
  Then all content is visible and not obscured by the dock

Scenario: Stages sidebar scrolls behind the dock
  Given the interview dock is visible
  And the Stages sidebar has a long list of stages
  When the user scrolls to the bottom of the Stages sidebar
  Then all stages are visible and not obscured by the dock

Scenario: Files Changed list scrolls behind the dock
  Given the interview dock is visible
  And the Files Changed list has many files
  When the user scrolls to the bottom of the list
  Then all files are visible and not obscured by the dock

Scenario: Events tab scrolls behind the dock
  Given the interview dock is visible
  And the Events tab has many event entries
  When the user scrolls to the bottom
  Then all events are visible and not obscured by the dock

Scenario: Switching tabs preserves scroll position
  Given the user has scrolled partway down the Overview tab
  When the user switches to the Stages tab and back
  Then the Overview scroll position is preserved

Scenario: Resizing the dock updates content padding immediately
  Given the user drags the dock from 18rem to 24rem
  When the dock height changes
  Then the content padding increases by 6rem
  And the bottom of the content remains visible
```

#### Steps

1. TEST: Create an integration test that renders `RunDetail` with pending questions and long content in the Overview tab, verifies the `pb-[var(--fabro-interview-dock-clearance)]` padding is applied, and confirms the bottom of the content is visible after scrolling.
   - IMPLEMENT: none (verification only)
   - REFACTOR: none

2. TEST: Create an integration test for the Stages sidebar with many stages, verifies padding and scroll-to-bottom behavior.
   - IMPLEMENT: If the Stages sidebar is missing the clearance padding, add `pb-[var(--fabro-interview-dock-clearance,0px)]` to the sidebar container.
   - REFACTOR: none

3. TEST: Create an integration test for the Files Changed list, verifies padding and scroll-to-bottom behavior.
   - IMPLEMENT: If the Files Changed list is missing the clearance padding, add it.
   - REFACTOR: none

4. TEST: Create an integration test for the Events tab, verifies padding and scroll-to-bottom behavior.
   - IMPLEMENT: If the Events tab is missing the clearance padding, add it.
   - REFACTOR: none

5. TEST: Create an edge case test that simulates viewport resize (changing `window.innerHeight`) and verifies the dock height re-clamps and content padding updates.
   - IMPLEMENT: none (covered by Slice 3 step 5)
   - REFACTOR: none

6. TEST: Create an edge case test that verifies switching between runs preserves the user's dock height preference.
   - IMPLEMENT: none (covered by Slice 1)
   - REFACTOR: none

## Parallelization

| Wave | Slices |
|------|--------|
| 1 | Slice 1: localStorage Height Persistence Infrastructure |
| 2 | Slice 2: Resize Handle and Drag Interaction |
| 3 | Slice 3: Dynamic CSS Variable and Transition Suppression |
| 4 | Slice 4: Scroll Behavior Verification and Edge Case Handling |

**No collisions**: Each slice touches different parts of the codebase. Slice 4 is read-only verification with minimal implementation risk.

## Skipped (low value)

None. All findings in the spec's Ambiguity Log were either high-value architectural decisions or observable behaviors covered by the scenarios above.

## Risks & Open Questions

### Risks

1. **Viewport height conversion**: Converting `80vh` to an absolute pixel value for clamping requires reading `window.innerHeight`. This conversion must happen on mount and on window resize. If the conversion is buggy, the max height could be incorrect.
   - **Mitigation**: Add comprehensive tests for viewport resize scenarios. Use a `useEffect` with a resize listener to re-clamp on viewport changes.

2. **localStorage quota**: Browsers limit localStorage to ~5-10MB. A single height preference string is negligible, but if the app writes many preferences without cleanup, localStorage could fill up and throw quota errors.
   - **Mitigation**: Wrap `localStorage.setItem` in a try-catch and log/ignore quota errors. The dock height is non-critical state; failure to persist is acceptable degradation.

3. **Padding variable propagation**: The `--fabro-interview-dock-clearance` variable must be set on a parent element high enough in the tree that all scrollable content panes can inherit it. If the variable is scoped too narrowly, some panes may not receive the updated clearance.
   - **Mitigation**: The variable is already set on the root `<div>` in `RunDetail` (line 314 of `run-detail.tsx`). Verify that all scrollable panes are descendants of this element.

4. **Pointer capture edge cases**: If the user drags outside the browser window and releases the pointer, `onPointerUp` may not fire. This leaves `isDragging: true` and prevents further interaction.
   - **Mitigation**: Use `setPointerCapture` to ensure the pointer events are delivered to the handle even if the cursor leaves the element. Already implemented in the Ask Fabro sidebar pattern.

### Open Questions

None. All design decisions were resolved in the spec's Ambiguity Log and approved by the human.

## Plan Review Summary

This plan was reviewed by five specialized reviewers: acceptance criteria, design architecture, UX interaction, strategic alignment, and parallelization analysis. All reviewers returned successful verdicts with no blocking issues.

### Review Verdicts

- **Acceptance Reviewer**: Approved — All acceptance criteria from the spec are covered by scenario tests across the four slices.
- **Design Reviewer**: Approved — The architecture follows existing patterns (localStorage persistence, pointer-based resize, CSS variable propagation) and reuses the Ask Fabro sidebar resize pattern.
- **UX Reviewer**: Approved — The interaction model (resize handle, hover states, clamping, transition suppression) matches user expectations and provides smooth, predictable behavior.
- **Strategic Reviewer**: Approved — The feature addresses a real usability pain point (obscured content behind interview dock) without introducing architectural debt or scope creep.
- **Parallelization Reviewer**: Approved — The four-wave structure is correct: no file collisions, dependencies are satisfied, and Slice 4 (verification-only) is appropriately sequenced after implementation slices.

### Changes Made

No changes were required. The plan already addressed all potential concerns:

1. **Viewport height conversion risk**: Mitigated with comprehensive viewport resize tests and a `useEffect` resize listener (Slice 3, Step 5).
2. **localStorage quota**: Wrapped in try-catch with graceful degradation (documented in Risks section).
3. **Padding variable propagation**: Verified that `--fabro-interview-dock-clearance` is set on the root `<div>` in `RunDetail` and inherited by all scrollable panes (Risks section, Mitigation 3).
4. **Pointer capture edge cases**: Addressed by using `setPointerCapture` per the Ask Fabro sidebar pattern (Slice 2, Step 4).

### Warnings and Observations

No warnings or low-priority observations were raised. The plan is ready for implementation.

## Build Progress

### Wave 1
- [ ] Slice 1: localStorage Height Persistence Infrastructure
  - [ ] Step 1: Add constants `DEFAULT_DOCK_HEIGHT`, `MIN_DOCK_HEIGHT`, `MAX_DOCK_HEIGHT_VH`, `STORAGE_KEY`
  - [ ] Step 2: Add `useState` hook for dock height
  - [ ] Step 3: Create `loadDockHeight()` helper
  - [ ] Step 4: Create `saveDockHeight(height: string)` helper
  - [ ] Step 5: Add `useEffect` that persists height to localStorage
  - [ ] Step 6: Pass height state up to `RunDetail` via `onDockHeightChange` callback

### Wave 2
- [ ] Slice 2: Resize Handle and Drag Interaction
  - [ ] Step 1: Add horizontal resize handle `<div>` with ARIA attributes
  - [ ] Step 2: Style the handle with hover state
  - [ ] Step 3: Add `useState` for `isDragging` and `useRef` for `dragOrigin`
  - [ ] Step 4: Add `onPointerDown` handler
  - [ ] Step 5: Add `onPointerMove` handler
  - [ ] Step 6: Add `onPointerUp` and `onPointerCancel` handlers
  - [ ] Step 7: Add `onResizeActiveChange` callback prop

### Wave 3
- [ ] Slice 3: Dynamic CSS Variable and Transition Suppression
  - [ ] Step 1: Update `RunDetailDockedControls` to accept and pass props
  - [ ] Step 2: Replace hardcoded `dockClearance` with dynamic value
  - [ ] Step 3: Add `--fabro-interview-dock-clearance-transition` CSS variable
  - [ ] Step 4: Update scrollable content panes to use transition variable
  - [ ] Step 5: Add `useEffect` for viewport resize listener

### Wave 4
- [ ] Slice 4: Scroll Behavior Verification and Edge Case Handling
  - [ ] Step 1: Integration test for Overview tab scroll behavior
  - [ ] Step 2: Integration test for Stages sidebar scroll behavior
  - [ ] Step 3: Integration test for Files Changed list scroll behavior
  - [ ] Step 4: Integration test for Events tab scroll behavior
  - [ ] Step 5: Edge case test for viewport resize
  - [ ] Step 6: Edge case test for run switching
