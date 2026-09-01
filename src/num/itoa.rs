//! Integer serialization.
//!
//! Digits are emitted two at a time from a 200-byte table, filling backwards
//! from the end of a stack buffer. `ilog10` gives the length up front so the
//! caller knows exactly how many bytes to copy out.

/// `"00010203...99"`, indexed by `2 * n`.
static DIGIT_PAIRS: &[u8; 200] = b"\
0001020304050607080910111213141516171819\
2021222324252627282930313233343536373839\
4041424344454647484950515253545556575859\
6061626364656667686970717273747576777879\
8081828384858687888990919293949596979899";

/// Widest decimal expansion of a `u64`.
pub(crate) const MAX_INT_DIGITS: usize = 20;

#[inline(always)]
fn digit_count(v: u64) -> usize {
    // `ilog10` lowers to a leading-zeros count plus a small table, not a loop.
    if v == 0 { 1 } else { v.ilog10() as usize + 1 }
}

/// Write the decimal form of `v` into `buf`, returning its length.
///
/// Filling backwards from the known end means no reversal pass.
///
/// The float writer generates digits a different way, splitting a fixed
/// sixteen-digit significand with SWAR (see `num::zmij`). That is the right
/// shape there because a float always has the full width to convert; integers
/// are usually short, and this loop leaves after an iteration or two.
#[inline]
pub(crate) fn write_u64(v: u64, buf: &mut [u8; MAX_INT_DIGITS]) -> usize {
    let n = digit_count(v);
    let mut p = n;
    let mut v = v;

    while v >= 100 {
        let rem = (v % 100) as usize;
        v /= 100;
        p -= 2;
        buf[p] = DIGIT_PAIRS[rem * 2];
        buf[p + 1] = DIGIT_PAIRS[rem * 2 + 1];
    }
    if v >= 10 {
        let rem = v as usize;
        p -= 2;
        buf[p] = DIGIT_PAIRS[rem * 2];
        buf[p + 1] = DIGIT_PAIRS[rem * 2 + 1];
    } else {
        p -= 1;
        buf[p] = b'0' + v as u8;
    }
    debug_assert_eq!(p, 0);
    n
}
