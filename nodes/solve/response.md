Implemented the fix in an Astropy checkout at `/home/daytona/workspace/astropy` on commit `b16c7d12ccbc7b2d20364b89fb44285bcbfede54`.

What changed:
- `astropy/nddata/mixins/ndarithmetic.py`
  - Fixed the mask propagation branch in `_arithmetic_mask`:
    - from `elif operand is None:`
    - to `elif operand.mask is None:`
- `astropy/nddata/mixins/tests/test_ndarithmetic.py`
  - Added a regression test covering `handle_mask=np.bitwise_or` when:
    - the other operand is a scalar
    - the other operand is an `NDDataRef` without a mask
    - both operand orders for the `NDDataRef` case

Why this fixes it:
- The bug was checking whether the operand object itself was `None`, instead of whether the operand’s `mask` was `None`.
- That caused custom mask handlers like `np.bitwise_or` to receive `None` as one mask input, triggering errors like:
  - `TypeError: unsupported operand type(s) for |: 'int' and 'NoneType'`

Validation:
- Reproduced the reported scenario successfully after the change.
- Ran:
  - `python -m pytest astropy/nddata/mixins/tests/test_ndarithmetic.py -k 'data_masks'`
  - `python -m pytest astropy/nddata/mixins/tests/test_ndarithmetic.py`
- Result: `181 passed`