Clean diff now. The fix is minimal - two early-return checks:

1. **`_return_list_of_arrays`** (line 1223-1224): After constructing the `xy` array from the list of axes, if it's empty, return a list of empty arrays (one per axis) without calling the C wcslib function.

2. **`_return_single_array`** (line 1241-1242): After validating the shape of the input `xy` array, if it's empty, return it as-is without calling the C wcslib function.

This prevents the `InconsistentAxisTypesError` from wcslib's C code which doesn't handle zero-length coordinate arrays. The fix correctly handles all three calling conventions:
- `wcs.wcs_pix2world([], [], 0)` — separate axis arrays
- `wcs.wcs_pix2world(np.array([]), np.array([]), 0)` — separate numpy arrays  
- `wcs.wcs_pix2world(np.empty((0, 2)), 0)` — single Nx2 array