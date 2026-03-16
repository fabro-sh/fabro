# Fix for NDDataRef Mask Propagation Bug (Issue #14961)

## Problem
In Astropy v5.3, NDDataRef mask propagation fails when one of the operands does not have a mask and a custom mask handling function like `np.bitwise_or` is used.

### Error Example
```python
>>> array = np.array([[0, 1, 0], [1, 0, 1], [0, 1, 0]])
>>> mask = np.array([[0, 1, 64], [8, 0, 1], [2, 1, 0]])
>>> nref_mask = NDDataRef(array, mask=mask)
>>> nref_mask.multiply(1., handle_mask=np.bitwise_or).mask
TypeError: unsupported operand type(s) for |: 'int' and 'NoneType'
```

## Root Cause
The `_arithmetic_mask` method in `NDArithmeticMixin` class did not properly handle the case where one operand has a mask and the other does not (mask is None). When checking for None masks, it would pass a None value to the `handle_mask` function, causing it to fail.

Specifically, the condition on line 523:
```python
elif operand is None:
```

Should also check if the operand's mask is None:
```python
elif operand is None or operand.mask is None:
```

## Solution
Modified the `_arithmetic_mask` method in `astropy/nddata/mixins/ndarithmetic.py` to properly handle the case where one operand has a mask and the other does not.

### Changes Made

#### File: `astropy/nddata/mixins/ndarithmetic.py`

**Line 523 (in the `_arithmetic_mask` method):**

Before:
```python
elif operand is None:
    return deepcopy(self.mask)
```

After:
```python
elif operand is None or operand.mask is None:
    return deepcopy(self.mask)
```

This ensures that when one operand does not have a mask (mask is None), the result simply copies the mask from the operand that has one, without passing None to the handle_mask function.

#### File: `astropy/nddata/mixins/tests/test_ndarithmetic.py`

Added comprehensive test `test_arithmetics_bitmask_one_operand_without_mask` that tests:
1. Mask with scalar operand (no mask)
2. Mask with NDData without mask
3. No mask with NDData with mask (reverse order)
4. Both operands with masks

This is a regression test to ensure this bug does not reoccur in future versions.

## Expected Behavior After Fix

All these cases now work correctly:
```python
>>> array = np.array([[0, 1, 0], [1, 0, 1], [0, 1, 0]])
>>> mask = np.array([[0, 1, 64], [8, 0, 1], [2, 1, 0]])
>>> nref_nomask = NDDataRef(array)
>>> nref_mask = NDDataRef(array, mask=mask)

# All of these now work:
>>> nref_mask.multiply(1., handle_mask=np.bitwise_or).mask
array([[ 0,  1, 64],
       [ 8,  0,  1],
       [ 2,  1,  0]])

>>> nref_mask.multiply(nref_nomask, handle_mask=np.bitwise_or).mask
array([[ 0,  1, 64],
       [ 8,  0,  1],
       [ 2,  1,  0]])

>>> nref_nomask.multiply(nref_mask, handle_mask=np.bitwise_or).mask
array([[ 0,  1, 64],
       [ 8,  0,  1],
       [ 2,  1,  0]])

>>> nref_mask.multiply(nref_mask, handle_mask=np.bitwise_or).mask
array([[ 0,  1, 64],
       [ 8,  0,  1],
       [ 2,  1,  0]])
```

## Impact
- Minimal code change (single line modification)
- Preserves backward compatibility
- Fixes bug in v5.3 arithmetic operations with custom mask handlers
- Enables proper bit-flag mask propagation for data reduction pipelines
