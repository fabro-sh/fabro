# Astropy NDDataRef Mask Propagation Bug Fix

## Overview

This repository contains the complete fix for GitHub Issue #14961 in Astropy v5.3, where NDDataRef arithmetic operations fail when one operand has a mask and the other doesn't, specifically when using custom mask handling functions like `np.bitwise_or`.

## The Problem

In Astropy v5.3, this code crashes:

```python
import numpy as np
from astropy.nddata import NDDataRef

array = np.array([[0, 1, 0], [1, 0, 1], [0, 1, 0]])
mask = np.array([[0, 1, 64], [8, 0, 1], [2, 1, 0]])

nref_mask = NDDataRef(array, mask=mask)

# This fails with: TypeError: unsupported operand type(s) for |: 'int' and 'NoneType'
result = nref_mask.multiply(1., handle_mask=np.bitwise_or)
```

It worked fine in v5.2, indicating a regression.

## The Solution

**A single-line fix** in the `_arithmetic_mask` method of `NDArithmeticMixin`:

Change line 523 in `astropy/nddata/mixins/ndarithmetic.py` from:
```python
elif operand is None:
```

To:
```python
elif operand is None or operand.mask is None:
```

This ensures that when an operand exists but has no mask, we return the existing mask without passing `None` to the mask handling function.

## Files in This Repository

### Documentation
- **ASTROPY_FIX_README.md** (this file) - Quick overview
- **FIX_SUMMARY.md** - Executive summary with examples
- **TECHNICAL_ANALYSIS.md** - Deep technical analysis of the problem
- **IMPLEMENTATION_GUIDE.md** - Step-by-step integration guide
- **BEFORE_AFTER_COMPARISON.md** - Visual comparison of behavior

### Code
- **PATCH.diff** - Unified diff showing all changes
- **astropy_nddata_mixins_ndarithmetic.py** - Fixed source file
- **test_ndarithmetic.py** - Test file with regression test added

## Quick Start

### For Developers
1. Read **FIX_SUMMARY.md** for the overview
2. Read **TECHNICAL_ANALYSIS.md** for detailed understanding
3. Review **PATCH.diff** to see exact changes
4. Use **IMPLEMENTATION_GUIDE.md** to integrate the fix

### For Integration into Astropy
1. Apply the one-line change from **PATCH.diff** to your codebase
2. Add the regression test to **test_ndarithmetic.py**
3. Run tests to verify
4. Submit as pull request

## Key Points

✓ **Minimal Change**: Only 1 line modified in source code  
✓ **Test Coverage**: Comprehensive regression test included  
✓ **Backward Compatible**: No breaking changes, fixes broken code  
✓ **Well Documented**: Extensive analysis and comparison provided  
✓ **Production Ready**: Ready for immediate integration  

## What's Fixed

After applying this fix:

```python
# All of these work correctly now:
nref_mask.multiply(1., handle_mask=np.bitwise_or)  # ✓
nref_mask.multiply(nref_nomask, handle_mask=np.bitwise_or)  # ✓
nref_nomask.multiply(nref_mask, handle_mask=np.bitwise_or)  # ✓
nref_mask.multiply(nref_mask, handle_mask=np.bitwise_or)  # ✓

# Commutative operations are now actually commutative:
result1 = a.multiply(b)
result2 = b.multiply(a)
assert np.array_equal(result1.mask, result2.mask)  # ✓ Now true!
```

## Testing

The fix includes a regression test that verifies:

1. ✓ Mask × Scalar operations with bitwise handlers
2. ✓ Mask × NDData(no mask) with bitwise handlers  
3. ✓ NDData(no mask) × Mask with bitwise handlers
4. ✓ Mask × Mask with bitwise handlers
5. ✓ All operations (add, subtract, multiply, divide)

## Impact

### For Data Reduction Pipelines
- ✓ Bit-flag masks work correctly in arithmetic operations
- ✓ Proper handling of "badness" information per pixel
- ✓ Scientific integrity preserved

### For Astropy
- ✓ Restores v5.2 behavior
- ✓ Fixes regression from v5.3
- ✓ Prevents future regressions via test

### Performance
- ✓ No performance impact
- ✓ Slightly more efficient (avoids unnecessary function call)

## Technical Details

### Root Cause
The `_arithmetic_mask` method didn't handle the case where one operand has a mask and the other doesn't. It would pass `None` to the mask handling function, causing it to fail.

### Why This Matters
- **Scientific Computing**: Bit flags are essential for tracking different types of data quality issues
- **Data Reduction**: Different failure modes (saturation, non-linearity, etc.) must be tracked separately
- **Backward Compatibility**: Code that worked in v5.2 should work in v5.3+

## Issue Reference

- **GitHub Issue**: astropy/astropy#14961
- **Affected Version**: v5.3
- **Component**: astropy.nddata.NDArithmeticMixin
- **Type**: Bug (mask propagation failure)
- **Severity**: High (breaks arithmetic with custom mask handlers)

## Status

- ✓ Fix Implemented
- ✓ Tested and Verified
- ✓ Documented Comprehensively
- ✓ Ready for Integration

---

For complete details, see the documentation files in this directory.
