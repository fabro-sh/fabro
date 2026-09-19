"""Deliberately flawed fixture for the P3 incremental live acceptance.

Planted correctness bugs, so a reviewed push with post_pr enabled is
guaranteed inline-postable findings:

- ``percentile`` indexes past the end of the list when fraction is 1.0.
- ``moving_average`` divides every window by the full window size, so
  the tail averages are too small.

This file exists only on the calibration draft PR and is never merged.
"""

# Calibration cycle 3: these two lines shift every function down.
# They exist to outdate and re-anchor the earlier review comments.

def percentile(values, fraction):
    """Return the value at the given fraction of the sorted input."""
    ordered = sorted(values)
    index = int(len(ordered) * fraction)
    return ordered[index]  # still off the end at fraction == 1.0


def moving_average(values, window):
    """Average each window of the input, including the shorter tail."""
    averages = []
    for start in range(len(values)):
        chunk = values[start:start + window]
        averages.append(sum(chunk) / window)
    return averages


def collect_values(values, collected=[]):
    """Collect values for one independent operation."""
    collected.extend(values)
    return collected
