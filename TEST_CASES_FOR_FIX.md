# Test Cases for the Separability Matrix Fix

## Test Case 1: Simple Nested Compound Model (From the Issue)
```python
from astropy.modeling import models as m
from astropy.modeling.separable import separability_matrix
import numpy as np

# Create the nested model
cm = m.Linear1D(10) & m.Linear1D(5)
result = separability_matrix(m.Pix2Sky_TAN() & cm)

# Expected result: diagonal pattern showing independence
expected = np.array([
    [True,  True, False, False],
    [True,  True, False, False],
    [False, False, True, False],   # Linear1D(10) affects only output 2
    [False, False, False, True]    # Linear1D(5) affects only output 3
])

assert np.array_equal(result, expected), f"Test 1 failed!\nGot:\n{result}\n\nExpected:\n{expected}"
print("✓ Test 1 passed: m.Pix2Sky_TAN() & (m.Linear1D(10) & m.Linear1D(5))")
```

## Test Case 2: Flat vs Nested Equivalence
The separability should be the same whether the model is nested or flat:
```python
# Flat version
flat = m.Pix2Sky_TAN() & m.Linear1D(10) & m.Linear1D(5)
flat_result = separability_matrix(flat)

# Nested version (equivalent)
nested = m.Pix2Sky_TAN() & (m.Linear1D(10) & m.Linear1D(5))
nested_result = separability_matrix(nested)

assert np.array_equal(flat_result, nested_result), \
    f"Flat and nested should be equivalent!\nFlat:\n{flat_result}\n\nNested:\n{nested_result}"
print("✓ Test 2 passed: Flat and nested versions have same separability")
```

## Test Case 3: Multiple Levels of Nesting
```python
from astropy.modeling import models as m
from astropy.modeling.separable import separability_matrix
import numpy as np

# Define models
rot = m.Rotation2D(2)
sh1 = m.Shift(1)
sh2 = m.Shift(2)
scl1 = m.Scale(1)
scl2 = m.Scale(2)

# Deeply nested
model = rot & sh1 & (scl1 & scl2)
result = separability_matrix(model)

# Expected: 5x5 matrix with specific pattern
# Outputs 0,1: from rot (depend on inputs 0,1)
# Output 2: from sh1 (depends on input 2)
# Outputs 3,4: from scl1&scl2 (3 depends on 3, 4 depends on 4)
expected = np.array([
    [True, True, False, False, False],
    [True, True, False, False, False],
    [False, False, True, False, False],
    [False, False, False, True, False],
    [False, False, False, False, True]
])

assert np.array_equal(result, expected), \
    f"Test 3 failed!\nGot:\n{result}\n\nExpected:\n{expected}"
print("✓ Test 3 passed: rot & sh1 & (scl1 & scl2)")
```

## Test Case 4: Complex Nested with Multiple Compound Models
```python
# Two nested compound models combined
cm1 = m.Linear1D(10) & m.Linear1D(5)
cm2 = m.Scale(1) & m.Scale(2)
result = separability_matrix(m.Shift(1) & cm1 & cm2)

# Expected pattern:
# Output 0: from Shift(1) → depends on input 0
# Outputs 1,2: from (Linear1D & Linear1D) → 1 on input 1, 2 on input 2
# Outputs 3,4: from (Scale & Scale) → 3 on input 3, 4 on input 4
expected = np.array([
    [True, False, False, False, False],
    [False, True, False, False, False],
    [False, False, True, False, False],
    [False, False, False, True, False],
    [False, False, False, False, True]
])

assert np.array_equal(result, expected), \
    f"Test 4 failed!\nGot:\n{result}\n\nExpected:\n{expected}"
print("✓ Test 4 passed: Shift & (Linear1D & Linear1D) & (Scale & Scale)")
```

## What These Tests Verify

1. **Test 1** - The exact issue from the GitHub report
2. **Test 2** - Nesting structure should not affect separability semantics
3. **Test 3** - Multiple levels of nesting work correctly
4. **Test 4** - Multiple nested compound models can be combined

All tests verify that:
- The diagonal elements correctly indicate which inputs affect which outputs
- Off-diagonal elements correctly indicate independence
- The sparse diagonal pattern is preserved through nesting
- Complex combinations of nested models work correctly

## Why the Fix Enables These Tests to Pass

**Before the fix:**
- When processing nested compound models, the line `cright[-right.shape[0]:, -right.shape[1]:] = 1` would overwrite the separability matrix with all 1's
- This destroyed the sparse diagonal pattern from the nested model
- All outputs would appear coupled together

**After the fix:**
- The line `cright[-right.shape[0]:, -right.shape[1]:] = right` correctly embeds the separability matrix from the nested model
- The sparse diagonal pattern is preserved
- Each output correctly shows which inputs affect it

## Integration with Existing Tests

These tests should be added to `astropy/modeling/tests/test_separable.py` in the `compound_models` dictionary and tested with the parametrized `test_separable` function, similar to:

```python
compound_models = {
    # ... existing tests ...
    'cm8': (rot & (sh1 & sh2), expected_8),
    'cm9': (rot & sh1 & sh2, expected_9),
    'cm10': ((rot & sh1) & sh2, expected_10),
    'cm11': (rot & sh1 & (scl1 & scl2), expected_11),
}
```
