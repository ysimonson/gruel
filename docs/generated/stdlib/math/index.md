# math

Mathematical helper functions over signed 32-bit integers.

This module is small on purpose — `i32` operators cover most of
what user code needs day-to-day. The wrappers here exist so users
can write `math.abs(x)` instead of inlining branches everywhere.

## Items

- [`fn abs`](fn.abs.md) — Returns the absolute value of an integer.
- [`fn min`](fn.min.md) — Returns the smaller of `a` and `b`. See also [max](fn.max.md) and [clamp](fn.clamp.md).
- [`fn max`](fn.max.md) — Returns the larger of `a` and `b`. See also [min](fn.min.md) and [clamp](fn.clamp.md).
- [`fn clamp`](fn.clamp.md) — Clamps `x` into the inclusive range `[lo, hi]`. Equivalent to
