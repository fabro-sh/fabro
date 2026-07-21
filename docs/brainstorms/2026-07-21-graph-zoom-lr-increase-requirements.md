# Specification: Increase Maximum Zoom for Left-to-Right Graph View

## Intent Description

Users working with workflow graphs in the left-to-right (LR) orientation currently experience a maximum zoom limit of 200%, which is insufficient for detailed inspection of complex graphs. The top-down (TB) orientation uses the same 200% limit, but the different aspect ratio and layout of TB graphs makes this limit feel more generous in practice. LR graphs, being horizontally oriented, often require higher magnification to read node labels and inspect edges comfortably.

This change increases the maximum zoom ceiling for the LR graph direction to provide parity in perceived usability between the two orientations. The goal is to allow users to zoom in further when viewing LR graphs while maintaining the existing zoom behavior for TB graphs. Additionally, to improve the user experience when switching between orientations, zoom levels are now persisted separately for each direction — when you switch from TB to LR and back, your previous zoom level for each mode is restored.

## Architecture Specification

### Affected Components

**`apps/fabro-web/app/lib/graph-viewport.ts`**
- Replace the single `GRAPH_MAX_ZOOM` constant (currently 200) with direction-aware zoom limits
- Export two constants: `GRAPH_MAX_ZOOM_TB = 200` and `GRAPH_MAX_ZOOM_LR = 400` (2x increase from 200%)
- Update `clampZoom()` to accept an optional direction parameter and apply the appropriate max limit
- Maintain backward compatibility: when direction is not specified, use the TB limit as the default

**`apps/fabro-web/app/routes/run-overview.tsx`**
- Thread the active direction (`activeDirection: "LR" | "TB"`) to `clampZoom()` calls
- Relevant call sites: `fitToWindow()` callback and zoom interactions via `zoomAtPoint()`
- Pass direction to `zoomAtPoint()` so it can forward it to `clampZoom()`
- Hold a separate remembered view per direction via `useRememberedGraphView`, keyed `<runId>-TB` and `<runId>-LR`
- Restore the appropriate zoom level when switching between TB and LR orientations

**`apps/fabro-web/app/components/graph-toolbar.tsx`**
- Update zoom button disabled state to use direction-aware max zoom
- Import both `GRAPH_MAX_ZOOM_TB` and `GRAPH_MAX_ZOOM_LR`
- Receive `direction` prop and check against the appropriate constant

**`apps/fabro-web/app/lib/graph-viewport.test.ts`**
- Update test assertions that reference `GRAPH_MAX_ZOOM` to test both TB and LR limits
- Add test coverage for direction-aware clamping behavior

### Type Signature Changes

```typescript
// Before
export const clampZoom = (zoom: number): number

// After
export const clampZoom = (zoom: number, direction?: "LR" | "TB"): number
```

```typescript
// Before
export function zoomAtPoint(
  view: GraphView,
  factor: number,
  cursor?: { x: number; y: number },
): GraphView

// After
export function zoomAtPoint(
  view: GraphView,
  factor: number,
  cursor?: { x: number; y: number },
  direction?: "LR" | "TB",
): GraphView
```

### Constraints

- **Backward compatibility**: `clampZoom()` and `zoomAtPoint()` must remain callable without the direction parameter for existing call sites outside the run overview route
- **Single source of truth**: Zoom limits must be defined as exported constants in `graph-viewport.ts`, not duplicated in component files
- **No change to TB behavior**: The TB max zoom remains 200%
- **Minimum zoom unchanged**: `GRAPH_MIN_ZOOM` remains 25% for both directions

## Acceptance Criteria

1. **LR zoom ceiling increased**: When viewing a workflow graph in LR orientation, the user can zoom in to 400% (up from 200%) using toolbar buttons, scroll wheel, or trackpad pinch gestures
2. **TB zoom ceiling unchanged**: When viewing a workflow graph in TB orientation, the maximum zoom remains 200%
3. **Toolbar button state**: The zoom-in button is disabled when at the direction-specific maximum (LR or TB), and the zoom-out button is disabled at 25% for both directions
4. **Fit-to-window respects limits**: The "Fit to window" button clamps the computed zoom to the direction-specific maximum when appropriate
5. **Zoom persistence per direction**: When switching from TB to LR (or vice versa), the zoom level you previously used for that direction is restored — each direction remembers its own zoom level independently
6. **Tests pass**: All existing `graph-viewport.test.ts` tests pass, and new tests verify direction-aware clamping behavior

## Ambiguity Log

| Decision | Classification | Resolved By | Rationale / Answer |
|----------|----------------|-------------|-------------------|
| What should the new LR maximum zoom be? | requires-stakeholder-input | human | Initially set to 300% (1.5x), but feedback requested increasing it to 400% (2x) for better usability with complex LR graphs. Updated to 400% based on user testing. |
| Should the zoom limit change be retroactive to existing remembered zoom states? | inferable | Spec author | When a user switches from TB to LR, their remembered zoom state for that run is already stored. Each direction now maintains its own zoom level in a ref, so switching between TB and LR preserves the zoom you used for each mode. The `clampZoom` behavior ensures each direction's zoom stays within its max limit. |
| Should the minimum zoom (25%) also be direction-aware? | inferable | Spec author | The user request specifically mentions "higher zoom in the overview graph in the left-to-right version" and compares max zoom between orientations. The minimum zoom was not mentioned as a problem. Changing the minimum would be scope creep. Keep `GRAPH_MIN_ZOOM` uniform at 25%. |
| Should the constants be named `GRAPH_MAX_ZOOM_TB` / `GRAPH_MAX_ZOOM_LR` or use a different naming convention? | inferable | Spec author | The existing constant is `GRAPH_MAX_ZOOM`. The direction-aware version should maintain lexical proximity and clarity. Suffix-based naming (`_TB`, `_LR`) follows the existing `Direction` type values and makes import/usage clear in components. Alternative approaches (e.g., a `MAX_ZOOM_BY_DIRECTION` map) add indirection without benefit. |
| Should `zoomAtPoint()` accept direction as a parameter or should callers pre-clamp? | inferable | Spec author | `zoomAtPoint()` internally calls `clampZoom()` at line 45. If direction-aware clamping is needed, threading direction through `zoomAtPoint()` avoids forcing every caller to manually clamp before calling. This centralizes the clamping logic and matches the existing function's responsibility. Pre-clamping at call sites would scatter the logic and risk inconsistency. |
| What happens to the playground canvas zoom (mentioned in graph-viewport.ts comments)? | inferable | Spec author | The comment at line 5-7 of `graph-viewport.ts` notes the playground canvas has its own pan/zoom with discrete steps and could import this module for cursor-anchored zoom. The playground is not mentioned in the user request. The change should not affect the playground unless it explicitly imports and uses the new direction-aware `clampZoom()`. Since it currently uses discrete steps (not the percentage-based system), it remains unaffected. |

---

## Cross-Artifact Consistency Gate

- [ ] Intent is unambiguous — two developers would interpret it the same way.
- [ ] Every behavior/goal in the intent maps to at least one acceptance criterion.
- [ ] Architecture constrains implementation without over-engineering.
- [ ] Same concepts named consistently across all three artifacts.
- [ ] No artifact contradicts another.
- [ ] Every gap/ambiguity finding is logged — inferable with rationale, or resolved by the human.
