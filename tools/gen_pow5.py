#!/usr/bin/env python3
"""Regenerate src/num/table.rs, the 128-bit power-of-five table used by the
Eisel-Lemire float parser.

Run from the repository root:  python3 tools/gen_pow5.py
"""
import os

LO, HI = -342, 308


def entry(q):
    """The 128 most significant bits of 5**q, normalized so bit 127 is set.

    The parser's saturation guard tests `lo == u64::MAX`, which detects a carry
    and therefore assumes the stored value never exceeds the true one. So every
    entry must be an under-estimate, with one exception: for -27 <= q < 0 the
    reciprocal of 5**-q needs no more than 64 bits, the rounded-up value is
    exact rather than approximate, and that range is inside the window the
    guard treats as always-safe.
    """
    if q >= 0:
        c = 5 ** q
        n = c.bit_length()
        b = c >> (n - 128) if n > 128 else c << (128 - n)
    elif q >= -27:
        c = 5 ** (-q)
        n = c.bit_length()
        b = ((1 << (127 + n)) // c) + 1
    else:
        # Compute well past 128 bits, then truncate, so the result is at most
        # the true value. Rounding up here would invert the sign of the error
        # the guard is written against.
        c = 5 ** (-q)
        n = c.bit_length()
        b = ((1 << (2 * n + 128)) // c) + 1
        while b >= (1 << 128):
            b >>= 1
    assert b >> 127 == 1 and b < (1 << 128), q
    return b & 0xFFFFFFFFFFFFFFFF, b >> 64


def main():
    assert entry(0) == (0, 0x8000000000000000)
    assert entry(-1) == (0xCCCCCCCCCCCCCCCD, 0xCCCCCCCCCCCCCCCC)
    assert entry(1) == (0, 0xA000000000000000)

    rows = "\n".join(
        "    (0x{:016x}, 0x{:016x}),".format(*entry(q)) for q in range(LO, HI + 1)
    )
    src = f'''//! 128-bit truncated powers of five, for Eisel-Lemire float parsing.
//!
//! Entry `q` holds the 128 most significant bits of `5^q`, normalized so the
//! top bit is set. Every entry is truncated, never rounded up, so the stored
//! value never exceeds the true one. That fixed error direction is what lets
//! the parser detect the rare cases where 128 bits are not enough to decide
//! the rounding: it only has to watch for a carry out of the low word.
//!
//! The exception is `-27 <= q < 0`, where the reciprocal fits in 64 bits and
//! the rounded-up value is exact. That range sits inside the exponent window
//! the parser already treats as always-safe.
//!
//! Generated, not hand written. See `tools/gen_pow5.py`.

/// Lowest decimal exponent in [`POWER_OF_FIVE_128`].
pub const SMALLEST_POWER_OF_FIVE: i32 = {LO};
/// Highest decimal exponent in [`POWER_OF_FIVE_128`].
pub const LARGEST_POWER_OF_FIVE: i32 = {HI};

/// `(low 64 bits, high 64 bits)` of the normalized `5^q`, indexed by
/// `q - SMALLEST_POWER_OF_FIVE`.
pub static POWER_OF_FIVE_128: [(u64, u64); {HI - LO + 1}] = [
{rows}
];
'''
    out = os.path.join(os.path.dirname(__file__), "..", "src", "num", "table.rs")
    with open(out, "w") as f:
        f.write(src)
    print(f"wrote {os.path.normpath(out)}: {HI - LO + 1} entries")


if __name__ == "__main__":
    main()
