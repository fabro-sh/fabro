Excellent! The fix is minimal, clean, and correct. Let me create a final comprehensive summary:

## Summary

I have successfully fixed **GitHub Issue #14241: NDDataRef mask propagation fails when one operand does not have a mask** in the Astropy repository.

### Problem
In Astropy v5.3, arithmetic operations on `NDDataRef` objects fail with `TypeError: unsupported operand type(s) for |: 'int' and 'NoneType'` when:
- One operand has a mask
- The other operand has no mask (mask=None)
- A custom mask handler like `np.bitwise_or` is used

### Root Cause
The `_arithmetic_mask` method in `astropy/nddata/mixins/ndarithmetic.py` was missing a check for `operand.mask is None`. It only checked `operand is None` (for collapse operations) but not `operand.mask is None` (when operand exists but has no mask). This caused the code to attempt calling `handle_mask(self.mask, None)`, which fails for bitwise operations.

### Solution
Added a single `elif` condition in the `_arithmetic_mask` method (lines 525-526):

```python
elif operand.mask is None:
    return deepcopy(self.mask)
```

This restores the v5.2 behavior where if one operand lacks a mask, the output simply copies the existing mask from the other operand.

### Files Modified

1. **astropy/nddata/mixins/ndarithmetic.py** (1 line added)
   - Added explicit check for `operand.mask is None` in `_arithmetic_mask` method

2. **astropy/nddata/mixins/tests/test_ndarithmetic.py** (1 comprehensive test added)
   - Added `test_arithmetics_mask_propagation_with_none_mask` with 6 test cases covering:
     - Arithmetic with constant when operand has mask
     - Mask multiplied by no mask
     - No mask multiplied by mask (commutative property verification)
     - Both operands having masks
     - Neither operand having mask
     - Boolean masks with logical_or

### Test Results
✅ **All 180 tests pass** (179 original + 1 new regression test)
✅ **All issue examples now work correctly**
✅ **Backward compatible** - restores v5.2 behavior
✅ **No breaking changes**