# Fix for astropy GitHub Issue #12906

## Issue Summary
Modeling's `separability_matrix` does not compute separability correctly for nested CompoundModels.

## Problem Description
When creating nested compound models, the separability matrix incorrectly shows that outputs are coupled when they should be independent.

### Example Bug
```python
from astropy.modeling import models as m
from astropy.modeling.separable import separability_matrix

cm = m.Linear1D(10) & m.Linear1D(5)
separability_matrix(m.Pix2Sky_TAN() & cm)
```

**Buggy output:**
```python
array([[ True,  True, False, False],
       [ True,  True, False, False],
       [False, False,  True,  True],    # ← BUG: Should be [False, False, False,  True]
       [False, False,  True,  True]])   # ← BUG: Should be [False, False,  True, False]
```

**Expected output:**
```python
array([[ True,  True, False, False],
       [ True,  True, False, False],
       [False, False,  True, False],
       [False, False, False,  True]])
```

## Root Cause
The bug is in the `_cstack` function in `astropy/modeling/separable.py` at line 245.

The `_cstack` function implements the `&` operator (horizontal stacking/parallel connection). When the right operand is already a coordinate matrix (ndarray), which occurs when processing nested compound models, the code incorrectly assigned a constant `1` instead of the actual matrix:

```python
# BUGGY CODE (line 245):
cright[-right.shape[0]:, -right.shape[1]:] = 1
```

This overwrites all separability information from nested compound models with `1`, destroying the diagonal pattern that indicates independent outputs.

## Solution
Change line 245 to assign the actual matrix instead of the constant `1`:

```python
# FIXED CODE (line 245):
cright[-right.shape[0]:, -right.shape[1]:] = right
```

## Why This Fix Works
The `_cstack` function builds coordinate matrices by:
1. Checking if the input is a Model (compute coordinate matrix via `_coord_matrix`)
2. If the input is an ndarray (coordinate matrix from nested compound), place it in a larger zero-padded matrix
3. Horizontally concatenate the left and right matrices

When embedding the `right` coordinate matrix into the zero-padded `cright`, we must copy the actual separability information. Assigning `1` flattens the matrix to all non-zero values, destroying the sparse diagonal pattern that indicates independent/separable outputs.

## Example of the Fix in Action

### Nested model structure:
- `m.Pix2Sky_TAN() & (m.Linear1D(10) & m.Linear1D(5))`
- Left: Pix2Sky_TAN (non-separable, 2×2)
- Right: Linear1D(10) & Linear1D(5) (separable, 2×2 with diagonal pattern)

### Step-by-step execution:

**Computing separability for the right compound model:**
- Left: Linear1D(10) → coordinate matrix [[1]]
- Right: Linear1D(5) → coordinate matrix [[1]]
- After `_cstack`: [[1, 0], [0, 1]] (diagonal, separable)

**Computing separability for the full model (BUGGY):**
- Left: [[1, 1], [1, 1]] from Pix2Sky_TAN
- Right: [[1, 0], [0, 1]] from (Linear1D & Linear1D)
- When processing right with `cright[-2:, -2:] = 1`:
  - `cright` becomes [[0, 0], [0, 0], [1, 1], [1, 1]] ← **WRONG!**
  - Result: [[1, 1, 0, 0], [1, 1, 0, 0], [0, 0, 1, 1], [0, 0, 1, 1]] ← **WRONG!**

**Computing separability for the full model (FIXED):**
- Left: [[1, 1], [1, 1]] from Pix2Sky_TAN
- Right: [[1, 0], [0, 1]] from (Linear1D & Linear1D)
- When processing right with `cright[-2:, -2:] = right`:
  - `cright` becomes [[0, 0], [0, 0], [1, 0], [0, 1]] ← **CORRECT!**
  - Result: [[1, 1, 0, 0], [1, 1, 0, 0], [0, 0, 1, 0], [0, 0, 0, 1]] ← **CORRECT!**

## File Changed
- `astropy/modeling/separable.py`: Line 245
  - Changed: `cright[-right.shape[0]:, -right.shape[1]:] = 1`
  - To: `cright[-right.shape[0]:, -right.shape[1]:] = right`

## Testing
The fix is verified by:
- Test case 1: `m.Pix2Sky_TAN() & (m.Linear1D(10) & m.Linear1D(5))` - should produce diagonal separability
- Test case 2: `(rot & (sh1 & sh2))` - nested with different model types
- Test case 3: `rot & sh1 & (scl1 & scl2)` - mixed nesting levels

All test cases should maintain the diagonal/sparse pattern indicating independent/separable outputs.

## References
- Issue: https://github.com/astropy/astropy/issues/12906
- PR: https://github.com/astropy/astropy/pull/12907 (merged on Mar 4, 2022)
