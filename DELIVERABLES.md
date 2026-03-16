# Deliverables: Astropy NDDataRef Mask Propagation Bug Fix

## Complete Package Contents

This fix package contains everything needed to understand, apply, and integrate the bug fix for Astropy issue #14961.

### 📚 Documentation Files

#### 1. **ASTROPY_FIX_README.md**
- Quick overview and summary
- Problem statement with code example
- Solution at a glance
- Integration checklist
- **Read this first for a quick understanding**

#### 2. **FIX_SUMMARY.md**
- Executive summary
- Detailed problem description with error examples
- Root cause explanation
- Solution details
- Expected behavior after fix
- Impact assessment
- **Read this for a complete but concise overview**

#### 3. **TECHNICAL_ANALYSIS.md**
- Deep technical dive into the problem
- Complete code location and context
- Bug analysis with code flow
- Why this matters
- Solution verification
- Files modified summary
- Testing strategy
- **Read this to fully understand the technical details**

#### 4. **IMPLEMENTATION_GUIDE.md**
- Step-by-step integration instructions
- Exact code changes with before/after
- Manual and unit testing procedures
- Integration steps checklist
- Validation checklist
- Expected test results
- **Follow this guide to apply the fix**

#### 5. **BEFORE_AFTER_COMPARISON.md**
- Visual comparison of buggy vs. fixed behavior
- Control flow diagrams
- Decision matrix for all cases
- Impact on commutative operations
- Test coverage comparison
- Regression prevention details
- **Use this to visualize the impact**

#### 6. **DELIVERABLES.md** (this file)
- Index of all files
- How to use each file
- Integration workflow
- Quality assurance checklist

### 💾 Source Code Files

#### 7. **PATCH.diff**
- Unified diff format showing all changes
- Can be applied with `patch` or `git apply`
- Format:
  - Line changed in `ndarithmetic.py` (1 line)
  - Regression test added to `test_ndarithmetic.py` (~40 lines)
- **Use this to apply the fix to your codebase**

#### 8. **astropy_nddata_mixins_ndarithmetic.py**
- Complete fixed source file
- Change at line 523
- Before: `elif operand is None:`
- After: `elif operand is None or operand.mask is None:`
- **Copy this file to replace the original if needed**

#### 9. **test_ndarithmetic.py**
- Complete test file with regression test added
- New test: `test_arithmetics_bitmask_one_operand_without_mask`
- Tests all 4 operations (add, subtract, multiply, divide)
- Tests all mask/no-mask combinations
- Uses bitwise_or handler to verify the fix
- **Add this test to prevent regression**

## How to Use This Package

### Scenario 1: Quick Understanding (5 minutes)
1. Read **ASTROPY_FIX_README.md**
2. Review the simple before/after in **FIX_SUMMARY.md**
3. Done!

### Scenario 2: Deep Understanding (30 minutes)
1. Read **ASTROPY_FIX_README.md**
2. Read **TECHNICAL_ANALYSIS.md** 
3. Review **BEFORE_AFTER_COMPARISON.md**
4. Examine **PATCH.diff**
5. Look at the code in **astropy_nddata_mixins_ndarithmetic.py** around line 523

### Scenario 3: Integration (15 minutes)
1. Follow steps in **IMPLEMENTATION_GUIDE.md**
2. Apply **PATCH.diff** to your repository
3. Or manually apply the one-line change
4. Add the regression test from **test_ndarithmetic.py**
5. Run tests
6. Verify with test cases in **FIX_SUMMARY.md**

### Scenario 4: Code Review
1. Read **TECHNICAL_ANALYSIS.md** for context
2. Review **PATCH.diff** for exact changes
3. Check **test_ndarithmetic.py** for test coverage
4. Use **BEFORE_AFTER_COMPARISON.md** to verify correctness

## File Dependencies

```
ASTROPY_FIX_README.md
├── FIX_SUMMARY.md
├── TECHNICAL_ANALYSIS.md
├── IMPLEMENTATION_GUIDE.md
├── BEFORE_AFTER_COMPARISON.md
└── PATCH.diff
    ├── astropy_nddata_mixins_ndarithmetic.py
    └── test_ndarithmetic.py
```

## Integration Checklist

- [ ] Read ASTROPY_FIX_README.md
- [ ] Review TECHNICAL_ANALYSIS.md
- [ ] Understand the fix in IMPLEMENTATION_GUIDE.md
- [ ] Apply PATCH.diff (or manual change from line 523)
- [ ] Add regression test from test_ndarithmetic.py
- [ ] Run test suite: `pytest astropy/nddata/mixins/tests/test_ndarithmetic.py::test_arithmetics_bitmask_one_operand_without_mask`
- [ ] Run full nddata tests: `pytest astropy/nddata/`
- [ ] Verify with manual test cases from FIX_SUMMARY.md
- [ ] Create pull request or commit to repository

## Quality Assurance Checklist

### Code Quality
- [ ] Syntax is valid (Python -m py_compile)
- [ ] Indentation matches existing code style
- [ ] No unused imports or variables
- [ ] Code follows Astropy conventions

### Testing
- [ ] Regression test passes
- [ ] All existing tests pass
- [ ] No new warnings or deprecations introduced
- [ ] Test covers all affected code paths

### Documentation
- [ ] Changes are documented
- [ ] Test has docstring
- [ ] No undocumented changes
- [ ] Issue reference is clear

### Backward Compatibility
- [ ] No breaking API changes
- [ ] No behavior changes for working code
- [ ] v5.2 test cases still pass
- [ ] Only fixes previously broken code

## The Fix at a Glance

**File**: `astropy/nddata/mixins/ndarithmetic.py`  
**Line**: 523  
**Change**: Add `or operand.mask is None` to condition  
**Before**: `elif operand is None:`  
**After**: `elif operand is None or operand.mask is None:`  
**Lines Changed**: 1  
**Lines Added (tests)**: ~40  
**Impact**: Fixes mask propagation for mixed mask operands

## Testing Command Reference

```bash
# Run just the new regression test
pytest astropy/nddata/mixins/tests/test_ndarithmetic.py::test_arithmetics_bitmask_one_operand_without_mask -v

# Run all mask-related tests
pytest astropy/nddata/mixins/tests/test_ndarithmetic.py -k "mask" -v

# Run all nddata tests
pytest astropy/nddata/ -v

# Run with coverage
pytest --cov=astropy.nddata astropy/nddata/mixins/tests/test_ndarithmetic.py
```

## Manual Test Verification

From `FIX_SUMMARY.md`:

```python
import numpy as np
from astropy.nddata import NDDataRef

array = np.array([[0, 1, 0], [1, 0, 1], [0, 1, 0]])
mask = np.array([[0, 1, 64], [8, 0, 1], [2, 1, 0]])

nref_nomask = NDDataRef(array)
nref_mask = NDDataRef(array, mask=mask)

# All should work without errors:
print(nref_mask.multiply(1., handle_mask=np.bitwise_or).mask)
print(nref_mask.multiply(nref_nomask, handle_mask=np.bitwise_or).mask)
print(nref_nomask.multiply(nref_mask, handle_mask=np.bitwise_or).mask)
print(nref_mask.multiply(nref_mask, handle_mask=np.bitwise_or).mask)
```

## Version Information

- **Astropy Version**: v5.3 (affected), v5.2 (working), future versions (fixed)
- **Python**: 3.8+
- **NumPy**: 1.18+

## Issue Tracking

- **GitHub Issue**: https://github.com/astropy/astropy/issues/14961
- **Issue Title**: "In v5.3, NDDataRef mask propagation fails when one of the operand does not have a mask"
- **Reporter**: @KathleenLabrie (original issue) and @chris-simpson (root cause identification)
- **Related PR**: #14175 (introduced the bug in v5.3)

## Quick Links

- [Astropy GitHub](https://github.com/astropy/astropy)
- [NDDataRef Documentation](https://docs.astropy.org/en/latest/nddata/index.html)
- [Issue #14961](https://github.com/astropy/astropy/issues/14961)

## Support

For questions about this fix:
1. Check the documentation files
2. Review the test cases
3. Refer to the issue discussion on GitHub
4. Examine the code comments in the fixed files

---

**Status**: ✓ Complete and Ready for Integration  
**Last Updated**: 2026-03-16  
**Version**: 1.0
