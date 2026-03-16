# Before and After Comparison

## The Bug in Action

### Scenario 1: Mask × Scalar with Bitwise OR
```python
import numpy as np
from astropy.nddata import NDDataRef

array = np.array([[0, 1, 0], [1, 0, 1], [0, 1, 0]])
mask = np.array([[0, 1, 64], [8, 0, 1], [2, 1, 0]])

nref_mask = NDDataRef(array, mask=mask)

# BEFORE FIX (v5.3):
>>> nref_mask.multiply(1., handle_mask=np.bitwise_or).mask
Traceback (most recent call last):
  ...
TypeError: unsupported operand type(s) for |: 'int' and 'NoneType'

# AFTER FIX:
>>> nref_mask.multiply(1., handle_mask=np.bitwise_or).mask
array([[ 0,  1, 64],
       [ 8,  0,  1],
       [ 2,  1,  0]])
```

### Scenario 2: Mask × NDData Without Mask
```python
nref_nomask = NDDataRef(array)

# BEFORE FIX (v5.3):
>>> nref_mask.multiply(nref_nomask, handle_mask=np.bitwise_or).mask
Traceback (most recent call last):
  ...
TypeError: unsupported operand type(s) for |: 'int' and 'NoneType'

# AFTER FIX:
>>> nref_mask.multiply(nref_nomask, handle_mask=np.bitwise_or).mask
array([[ 0,  1, 64],
       [ 8,  0,  1],
       [ 2,  1,  0]])
```

### Scenario 3: NDData Without Mask × Mask (Order Reversed!)
```python
# BEFORE FIX (v5.3):
>>> nref_nomask.multiply(nref_mask, handle_mask=np.bitwise_or).mask
array([[ 0,  1, 64],
       [ 8,  0,  1],
       [ 2,  1,  0]])
# ^ Works! But only because of asymmetry in the buggy code

# AFTER FIX:
>>> nref_nomask.multiply(nref_mask, handle_mask=np.bitwise_or).mask
array([[ 0,  1, 64],
       [ 8,  0,  1],
       [ 2,  1,  0]])
# ^ Now commutative operations are actually commutative!
```

## Code Changes

### The Problem Area in ndarithmetic.py

#### BEFORE (v5.3 - Buggy):
```python
def _arithmetic_mask(self, operation, operand, handle_mask, axis=None, **kwds):
    """
    Calculate the resulting mask.
    ...
    """
    # If only one mask is present we need not bother about any type checks
    if (
        self.mask is None and operand is not None and operand.mask is None
    ) or handle_mask is None:
        return None
    elif self.mask is None and operand is not None:
        # Make a copy so there is no reference in the result.
        return deepcopy(operand.mask)
    elif operand is None:  # ← BUG: Doesn't check operand.mask
        return deepcopy(self.mask)
    else:
        # Now lets calculate the resulting mask (operation enforces copy)
        # ↓ Can pass None as operand.mask here!
        return handle_mask(self.mask, operand.mask, **kwds)
```

#### AFTER (Fixed):
```python
def _arithmetic_mask(self, operation, operand, handle_mask, axis=None, **kwds):
    """
    Calculate the resulting mask.
    ...
    """
    # If only one mask is present we need not bother about any type checks
    if (
        self.mask is None and operand is not None and operand.mask is None
    ) or handle_mask is None:
        return None
    elif self.mask is None and operand is not None:
        # Make a copy so there is no reference in the result.
        return deepcopy(operand.mask)
    elif operand is None or operand.mask is None:  # ← FIXED: Now checks operand.mask
        return deepcopy(self.mask)
    else:
        # Now lets calculate the resulting mask (operation enforces copy)
        # ✓ Guaranteed both masks are not None here
        return handle_mask(self.mask, operand.mask, **kwds)
```

## Control Flow Comparison

### Before Fix: Mixed Mask Case (self.mask ≠ None, operand.mask = None)

```
Start: _arithmetic_mask(self.mask=[...], operand.mask=None, handle_mask=np.bitwise_or)

Condition 1: self.mask is None and operand is not None and operand.mask is None
             False and True and True = False  ✗ Skip

Condition 2: self.mask is None and operand is not None  
             False and True = False  ✗ Skip

Condition 3: operand is None
             False  ✗ Skip

→ Falls through to else block
  Calls: handle_mask(self.mask=[...], operand.mask=None, **kwds)
         = np.bitwise_or([...], None)  
         ✗ CRASH: "unsupported operand type(s) for |: 'int' and 'NoneType'"
```

### After Fix: Mixed Mask Case (self.mask ≠ None, operand.mask = None)

```
Start: _arithmetic_mask(self.mask=[...], operand.mask=None, handle_mask=np.bitwise_or)

Condition 1: self.mask is None and operand is not None and operand.mask is None
             False and True and True = False  ✗ Skip

Condition 2: self.mask is None and operand is not None  
             False and True = False  ✗ Skip

Condition 3: operand is None or operand.mask is None
             False or True = True  ✓ MATCH!
             
→ Execute: return deepcopy(self.mask) = deepcopy([...])
  ✓ SUCCESS: Returns mask array as expected
```

## Decision Matrix

### All Possible Cases

| Case | self.mask | operand | operand.mask | v5.3 Behavior | Fixed Behavior | Result |
|------|-----------|---------|--------------|---------------|----------------|--------|
| 1 | None | None | None | Return None | Return None | ✓ Same |
| 2 | None | NDData | Mask | Return Mask | Return Mask | ✓ Same |
| 3 | Mask | None | N/A | Return Mask | Return Mask | ✓ Same |
| 4 | Mask | NDData | None | **CRASH** | Return Mask | **FIXED** |
| 5 | Mask | NDData | Mask | Call handle_mask | Call handle_mask | ✓ Same |

## Impact on Commutative Operations

Arithmetic operations should be commutative with respect to mask handling:

### Multiplication Example (a × b should have same mask result as b × a)

```python
array_a = np.array([1, 2, 3])
array_b = np.array([4, 5, 6])
mask_a = np.array([0, 1, 0])
mask_b = None

nd_a = NDDataRef(array_a, mask=mask_a)
nd_b = NDDataRef(array_b, mask=mask_b)

# BEFORE FIX:
result1 = nd_a.multiply(nd_b, handle_mask=np.bitwise_or)
# ✓ Works because nd_a.mask comes first in handle_mask call

result2 = nd_b.multiply(nd_a, handle_mask=np.bitwise_or)  
# ✗ CRASH: np.bitwise_or(None, mask_a) fails

# AFTER FIX:
result1 = nd_a.multiply(nd_b, handle_mask=np.bitwise_or)
# ✓ Returns mask_a

result2 = nd_b.multiply(nd_a, handle_mask=np.bitwise_or)
# ✓ Returns mask_a (same as result1!)

assert np.array_equal(result1.mask, result2.mask)  # ✓ PASSES
```

## Test Coverage

### Before Fix
- Tests with both masks present ✓
- Tests with both masks None ✓
- Tests with scalar operands ✓
- **MISSING**: Tests with mixed mask cases ✗

### After Fix
- All previous tests ✓
- **NEW**: `test_arithmetics_bitmask_one_operand_without_mask`
  - Tests: Mask × Scalar ✓
  - Tests: Mask × NDData(no mask) ✓
  - Tests: NDData(no mask) × Mask ✓
  - Tests: Mask × Mask ✓
  - All 4 operations (add, subtract, multiply, divide) ✓
  - Custom handler (np.bitwise_or) ✓

## Regression Prevention

The fix includes comprehensive test coverage to prevent regression:

```python
@pytest.mark.parametrize("meth", ["add", "subtract", "divide", "multiply"])
def test_arithmetics_bitmask_one_operand_without_mask(meth):
    """Test mask propagation when one operand does not have a mask."""
    # Tests all combinations of mask/no-mask with all arithmetic operations
    # This test would FAIL on v5.3 without the fix
    # This test PASSES with the fix applied
```

Future versions of Astropy will have this test that would catch any similar regressions.
