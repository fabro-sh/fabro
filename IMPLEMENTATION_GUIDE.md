# Implementation Guide: Applying the NDDataRef Mask Propagation Fix

## Quick Summary
This fix addresses a critical bug in Astropy v5.3 where NDDataRef arithmetic operations fail when one operand has a mask and the other does not, when using custom mask handling functions like `np.bitwise_or`.

**Changes**: 1 line in source code + test coverage

## Exact Change Required

### File: `astropy/nddata/mixins/ndarithmetic.py`
Line: 523

**Before:**
```python
        elif operand is None:
            return deepcopy(self.mask)
```

**After:**
```python
        elif operand is None or operand.mask is None:
            return deepcopy(self.mask)
```

## Why This Single Change Fixes Everything

The `_arithmetic_mask` method is responsible for propagating masks during arithmetic operations. When it encounters an operand without a mask (mask=None), it should return the mask of the other operand without attempting to call the `handle_mask` function.

The bug was that the code only checked `operand is None` (when operand parameter is missing entirely), but didn't check `operand.mask is None` (when operand exists but has no mask).

This single condition addition handles all cases:
- ✓ Operand is None (collapse operations like sum, mean)
- ✓ Operand exists but mask is None (mixed mask operations)

## Testing the Fix

### Manual Test (Before and After)
```python
import numpy as np
from astropy.nddata import NDDataRef

array = np.array([[0, 1, 0], [1, 0, 1], [0, 1, 0]])
mask = np.array([[0, 1, 64], [8, 0, 1], [2, 1, 0]])

nref_nomask = NDDataRef(array)
nref_mask = NDDataRef(array, mask=mask)

# Before fix: TypeError
# After fix: Works correctly
result = nref_mask.multiply(1., handle_mask=np.bitwise_or)
print(result.mask)  # Should print the mask array

result2 = nref_mask.multiply(nref_nomask, handle_mask=np.bitwise_or)
print(result2.mask)  # Should print the mask array

result3 = nref_nomask.multiply(nref_mask, handle_mask=np.bitwise_or)
print(result3.mask)  # Should print the mask array
```

### Unit Test
A new regression test `test_arithmetics_bitmask_one_operand_without_mask` should be added to 
`astropy/nddata/mixins/tests/test_ndarithmetic.py` to prevent this regression in future versions.

The test verifies all four arithmetic operations (add, subtract, multiply, divide) work correctly with mixed mask operands.

## Files Provided

1. **FIX_SUMMARY.md** - Overview and examples
2. **TECHNICAL_ANALYSIS.md** - Deep dive into the problem and solution
3. **IMPLEMENTATION_GUIDE.md** - This file
4. **PATCH.diff** - Complete diff showing all changes
5. **astropy_nddata_mixins_ndarithmetic.py** - Fixed source file
6. **test_ndarithmetic.py** - Test file with regression test added

## Integration Steps

1. Apply the one-line change to `astropy/nddata/mixins/ndarithmetic.py`
2. Add the regression test to `astropy/nddata/mixins/tests/test_ndarithmetic.py`
3. Run the test suite: `pytest astropy/nddata/mixins/tests/test_ndarithmetic.py::test_arithmetics_bitmask_one_operand_without_mask -v`
4. Run full nddata tests: `pytest astropy/nddata/` to ensure no regressions

## Validation Checklist

- [ ] Single line change applied to `_arithmetic_mask` method
- [ ] Regression test added
- [ ] Test passes: `pytest ...::test_arithmetics_bitmask_one_operand_without_mask`
- [ ] Full test suite passes: `pytest astropy/nddata/`
- [ ] Manual test cases work (see above)
- [ ] Backward compatibility verified (no v5.2 tests should fail)

## Expected Test Results

All tests should pass:
```
test_arithmetics_bitmask_one_operand_without_mask[add] PASSED
test_arithmetics_bitmask_one_operand_without_mask[subtract] PASSED
test_arithmetics_bitmask_one_operand_without_mask[divide] PASSED
test_arithmetics_bitmask_one_operand_without_mask[multiply] PASSED
```

## Impact on Users

### Before Fix (v5.3)
```python
>>> nref_mask.multiply(1., handle_mask=np.bitwise_or)
TypeError: unsupported operand type(s) for |: 'int' and 'NoneType'
```

### After Fix
```python
>>> nref_mask.multiply(1., handle_mask=np.bitwise_or)
NDDataRef(...with mask array as expected...)
```

## Performance

No performance impact - the fix is a simple condition check that avoids an unnecessary function call.

## Questions?

Refer to:
- Issue #14961 on GitHub Astropy
- Pull request comment mentioning the `operand is None` vs `operand.mask is None` distinction
- The comprehensive test case for expected behavior
