# Plan: Increase Maximum Zoom for Left-to-Right Graph View

**Status**: approved
**Spec**: docs/specs/graph-zoom-lr-increase.md

## Goal

Enable users to zoom in further when viewing workflow graphs in left-to-right (LR) orientation by increasing the maximum zoom ceiling from 200% to 300%, while maintaining the existing 200% maximum for top-down (TB) orientation.

## Acceptance Criteria

1. **LR zoom ceiling increased**: When viewing a workflow graph in LR orientation, the user can zoom in to 300% (up from 200%) using toolbar buttons, scroll wheel, or trackpad pinch gestures
2. **TB zoom ceiling unchanged**: When viewing a workflow graph in TB orientation, the maximum zoom remains 200%
3. **Toolbar button state**: The zoom-in button is disabled when at the direction-specific maximum (LR or TB), and the zoom-out button is disabled at 25% for both directions
4. **Fit-to-window respects limits**: The "Fit to window" button clamps the computed zoom to the direction-specific maximum when appropriate
5. **Direction switching preserves zoom**: Switching from LR to TB when zoomed above 200% clamps the zoom to 200%; switching from TB to LR restores the ability to zoom above 200% but does not automatically increase the zoom level
6. **Tests pass**: All existing `graph-viewport.test.ts` tests pass, and new tests verify direction-aware clamping behavior

## Slices

### Slice 1: Direction-aware zoom constants and clampZoom function

**Depends-on**: none
**Files**: `apps/fabro-web/app/lib/graph-viewport.ts`, `apps/fabro-web/app/lib/graph-viewport.test.ts`

Replace the single `GRAPH_MAX_ZOOM` constant with direction-aware constants and update `clampZoom()` to accept an optional direction parameter.

#### Scenarios

**Scenario: TB zoom clamping**
- Given the zoom direction is TB
- When clamping zoom at 150%
- Then the zoom remains 150%
- When clamping zoom at 250%
- Then the zoom is clamped to 200%

**Scenario: LR zoom clamping**
- Given the zoom direction is LR
- When clamping zoom at 250%
- Then the zoom remains 250%
- When clamping zoom at 350%
- Then the zoom is clamped to 300%

**Scenario: Backward compatibility with no direction**
- When clamping zoom at 150% without specifying direction
- Then the zoom remains 150%
- When clamping zoom at 250% without specifying direction
- Then the zoom is clamped to 200% (TB default)

**Scenario: Minimum zoom unchanged**
- Given the zoom direction is TB
- When clamping zoom at 10%
- Then the zoom is clamped to 25%
- Given the zoom direction is LR
- When clamping zoom at 10%
- Then the zoom is clamped to 25%

#### Steps

1. IMPLEMENT: Export `GRAPH_MAX_ZOOM_TB = 200` and `GRAPH_MAX_ZOOM_LR = 300` constants in `graph-viewport.ts`, replacing `GRAPH_MAX_ZOOM`
2. TEST: Add unit tests verifying both constants have the expected values (200 and 300)
3. REFACTOR: Ensure export names are clear and aligned with spec naming convention
4. IMPLEMENT: Update `clampZoom(zoom: number, direction?: "LR" | "TB"): number` to select max based on direction, defaulting to TB
5. TEST: Add unit tests for `clampZoom()` with TB direction (max 200)
6. TEST: Add unit tests for `clampZoom()` with LR direction (max 300)
7. TEST: Add unit test for `clampZoom()` without direction parameter (backward compat, defaults to 200)
8. TEST: Add unit test verifying minimum zoom (25%) applies to both directions
9. REFACTOR: Review test coverage for edge cases (exactly at limits, just under, just over)

---

### Slice 2: Direction-aware zoomAtPoint function

**Depends-on**: Slice 1
**Files**: `apps/fabro-web/app/lib/graph-viewport.ts`, `apps/fabro-web/app/lib/graph-viewport.test.ts`

Thread the direction parameter through `zoomAtPoint()` so it can forward it to the now direction-aware `clampZoom()`.

#### Scenarios

**Scenario: zoomAtPoint with TB direction clamps to 200%**
- Given a view at 180% zoom
- And the direction is TB
- When zooming by factor 1.5 at the center
- Then the zoom is clamped to 200%
- And the pan adjusts based on the clamped ratio (k = 200/180)

**Scenario: zoomAtPoint with LR direction clamps to 300%**
- Given a view at 250% zoom
- And the direction is LR
- When zooming by factor 1.5 at the center
- Then the zoom is clamped to 300%
- And the pan adjusts based on the clamped ratio (k = 300/250)

**Scenario: zoomAtPoint without direction defaults to TB**
- Given a view at 180% zoom
- When zooming by factor 1.5 at the center without specifying direction
- Then the zoom is clamped to 200%

**Scenario: Cursor-anchored zoom respects direction-aware clamping**
- Given a view at 280% zoom in LR direction
- And a cursor at offset (50, 40)
- When zooming by factor 1.2
- Then the zoom is clamped to 300%
- And the content point under the cursor remains anchored after clamping

#### Steps

1. IMPLEMENT: Update `zoomAtPoint()` signature to accept optional `direction?: "LR" | "TB"` parameter
2. IMPLEMENT: Pass `direction` to the `clampZoom()` call inside `zoomAtPoint()`
3. TEST: Update existing test "clamps zoom and applies the clamped ratio to pan" to verify TB behavior (max 200)
4. TEST: Add test for LR clamping in `zoomAtPoint()` (max 300, verify pan adjustment with k = 300/initial)
5. TEST: Add test for `zoomAtPoint()` without direction parameter (backward compat, max 200)
6. TEST: Add test for cursor-anchored zoom with LR direction near the 300% limit
7. REFACTOR: Verify all `zoomAtPoint()` tests check both zoom and pan outcomes, not just zoom

---

### Slice 3: Thread direction to run-overview route zoom interactions

**Depends-on**: Slice 2
**Files**: `apps/fabro-web/app/routes/run-overview.tsx`

Update the run overview route to pass `activeDirection` to `zoomAtPoint()` and `clampZoom()` calls so wheel/pinch zoom and fit-to-window honor the direction-aware limits.

#### Scenarios

**Scenario: Wheel zoom in LR direction respects 300% max**
- Given a workflow graph in LR orientation at 250% zoom
- When the user performs Ctrl+scroll to zoom in
- Then the zoom increases toward 300% and stops at 300%

**Scenario: Wheel zoom in TB direction respects 200% max**
- Given a workflow graph in TB orientation at 180% zoom
- When the user performs Ctrl+scroll to zoom in
- Then the zoom increases toward 200% and stops at 200%

**Scenario: Fit-to-window in LR clamps to 300%**
- Given a tiny workflow graph that would fit at 500% zoom
- And the direction is LR
- When the user clicks "Fit to window"
- Then the zoom is set to 300% (clamped)

**Scenario: Fit-to-window in TB clamps to 200%**
- Given a tiny workflow graph that would fit at 500% zoom
- And the direction is TB
- When the user clicks "Fit to window"
- Then the zoom is set to 200% (clamped)

**Scenario: Switching from LR to TB when zoomed above 200% clamps to 200%**
- Given a workflow graph in LR orientation at 280% zoom
- When the user switches to TB orientation
- Then the zoom is clamped to 200%

**Scenario: Switching from TB to LR does not auto-increase zoom**
- Given a workflow graph in TB orientation at 150% zoom
- When the user switches to LR orientation
- Then the zoom remains 150% (ability to zoom to 300% is available, but zoom does not change)

#### Steps

1. IMPLEMENT: Update the `onWheel` callback's `zoomAtPoint()` call (line 119) to pass `activeDirection`
2. IMPLEMENT: Update the `fitToWindow` callback's `clampZoom()` call (line 140) to pass `activeDirection`
3. TEST: Manual verification: open a run, switch to LR, zoom to 280% via wheel, verify it clamps at 300%
4. TEST: Manual verification: open a run, switch to TB, zoom to 180% via wheel, verify it clamps at 200%
5. TEST: Manual verification: create a tiny graph, switch to LR, click fit-to-window, verify zoom clamps to 300% if computed fit exceeds it
6. TEST: Manual verification: switch from LR at 280% to TB, verify zoom clamps to 200%
7. TEST: Manual verification: switch from TB at 150% to LR, verify zoom stays at 150%
8. REFACTOR: Review all `zoomAtPoint()` call sites in the file to ensure none were missed

---

### Slice 4: Thread direction to graph toolbar zoom buttons

**Depends-on**: Slice 1, Slice 2, Slice 3
**Files**: `apps/fabro-web/app/components/graph-toolbar.tsx`, `apps/fabro-web/app/routes/run-overview.tsx`

Update the graph toolbar to receive the `direction` prop and thread it to the `onZoomBy()` callback, and update the zoom-in button's disabled state to check the direction-aware max.

#### Scenarios

**Scenario: Zoom-in button in LR disables at 300%**
- Given a workflow graph in LR orientation at 300% zoom
- Then the zoom-in button is disabled

**Scenario: Zoom-in button in TB disables at 200%**
- Given a workflow graph in TB orientation at 200% zoom
- Then the zoom-in button is disabled

**Scenario: Zoom-in button in LR is enabled below 300%**
- Given a workflow graph in LR orientation at 250% zoom
- Then the zoom-in button is enabled

**Scenario: Zoom-in button in TB is enabled below 200%**
- Given a workflow graph in TB orientation at 150% zoom
- Then the zoom-in button is enabled

**Scenario: Zoom-out button disables at 25% for both directions**
- Given a workflow graph in LR orientation at 25% zoom
- Then the zoom-out button is disabled
- Given a workflow graph in TB orientation at 25% zoom
- Then the zoom-out button is disabled

**Scenario: Toolbar zoom-in button in LR respects 300% limit**
- Given a workflow graph in LR orientation at 280% zoom
- When the user clicks the zoom-in button
- Then the zoom increases but stops at 300%

**Scenario: Toolbar zoom-in button in TB respects 200% limit**
- Given a workflow graph in TB orientation at 180% zoom
- When the user clicks the zoom-in button
- Then the zoom increases but stops at 200%

#### Steps

1. IMPLEMENT: Import `GRAPH_MAX_ZOOM_TB` and `GRAPH_MAX_ZOOM_LR` in `graph-toolbar.tsx`, remove `GRAPH_MAX_ZOOM` import
2. IMPLEMENT: Update `GraphToolbar` props to accept `direction: Direction`
3. IMPLEMENT: Update zoom-in button's `disabled` condition to check `zoom >= (direction === "LR" ? GRAPH_MAX_ZOOM_LR : GRAPH_MAX_ZOOM_TB)`
4. IMPLEMENT: Update `run-overview.tsx` `<GraphToolbar>` call to pass `direction={activeDirection}` prop
5. IMPLEMENT: Update `run-overview.tsx` `onZoomBy` callback to pass `activeDirection` to `zoomAtPoint()`: `onZoomBy={(factor) => setView((v) => zoomAtPoint(v, factor, undefined, activeDirection))}`
6. TEST: Manual verification: open run in LR at 250%, verify zoom-in button is enabled
7. TEST: Manual verification: zoom to 300% in LR, verify zoom-in button is disabled
8. TEST: Manual verification: open run in TB at 180%, verify zoom-in button is enabled
9. TEST: Manual verification: zoom to 200% in TB, verify zoom-in button is disabled
10. TEST: Manual verification: click zoom-in button in LR at 280%, verify zoom goes to 300% and button disables
11. TEST: Manual verification: click zoom-in button in TB at 180%, verify zoom goes to 200% and button disables
12. REFACTOR: Verify toolbar component remains stateless and all logic is prop-driven

---

## Parallelization

### Wave 1
- Slice 1: Direction-aware zoom constants and clampZoom function

### Wave 2
- Slice 2: Direction-aware zoomAtPoint function

### Wave 3
- Slice 3: Thread direction to run-overview route zoom interactions

### Wave 4
- Slice 4: Thread direction to graph toolbar zoom buttons

**No collisions**: Each slice touches distinct portions of the files. Slice 1 and 2 modify `graph-viewport.ts` and its tests but are sequential (Slice 2 depends on Slice 1). Slice 3 and 4 both modify `run-overview.tsx` but are sequential (Slice 4 depends on Slice 3). No same-wave slices overlap files.

## Skipped (low value)

None. All acceptance criteria map to testable scenarios with observable outcomes. The spec's Ambiguity Log entries are all resolved and do not contain low-value items to skip.

## Risks & Open Questions

### Risks

1. **Performance at 300% zoom**: Rendering performance at 300% zoom for large graphs has not been validated. The spec assumes 1.5x increase is conservative enough, but this should be monitored during manual testing. If performance degrades, the LR max may need adjustment.

2. **Remembered zoom state edge case**: The `useRememberedGraphView` hook persists zoom per run. If a user zooms to 280% in LR, switches to another run, then switches back and toggles to TB, the zoom will clamp to 200%. The remembered state is not direction-specific. This is expected behavior per the spec (AC #5), but users may perceive it as losing their zoom level.

3. **Test coverage gap**: The plan includes manual verification steps because there are no existing integration or E2E tests for the graph toolbar and zoom interactions. Adding automated tests for these would improve confidence but is out of scope for this feature (no existing test infrastructure for React component interactions in this codebase).

### Open Questions

None. All ambiguities were resolved in the spec's Ambiguity Log. The 1.5x multiplier (300% max for LR) was confirmed by stakeholder input. All implementation details are constrained by the architecture specification.

## Plan Review Summary

**Reviewers**: review_acceptance, review_design, review_ux, review_strategic, review_parallel

**Verdict**: All five reviewers approved the plan with no blockers.

### Review Outcomes

- **review_acceptance**: succeeded — acceptance criteria fully covered by scenarios
- **review_design**: succeeded — design approach validated
- **review_ux**: succeeded — user experience flow approved
- **review_strategic**: succeeded — alignment with product strategy confirmed
- **review_parallel**: succeeded — parallelization plan verified (no wave collisions, valid dependencies)

### Changes Made

None. The plan had no blockers or critical warnings requiring changes.

### Warnings and Observations

No warnings or observations were raised by the reviewers. The plan's test strategy (mix of automated unit tests and manual verification) was accepted as appropriate given the current test infrastructure. The direction-aware zoom limit architecture was validated against the spec's acceptance criteria.
