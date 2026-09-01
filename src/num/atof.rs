//! Float parsing: Eisel-Lemire, with an exact fast path and a fallback.
//!
//! Three tiers, in the order they are attempted:
//!
//! 1. **Exact.** A mantissa under 2^53 with a small decimal exponent is one
//!    hardware multiply or divide away from the correct answer, because both
//!    operands and the result are exactly representable.
//! 2. **Eisel-Lemire.** A 64x128 multiply against a table of truncated powers
//!    of five decides the rounding for essentially every real input. See
//!    <https://arxiv.org/abs/2101.11408>.
//! 3. **Fallback.** For the handful of inputs where 128 bits cannot resolve a
//!    tie, defer to the standard library, which does full big-integer
//!    arithmetic. This is correct by construction and effectively never runs.

use super::table::{LARGEST_POWER_OF_FIVE, POWER_OF_FIVE_128, SMALLEST_POWER_OF_FIVE};
use crate::error::{ErrorCode, PResult};
use crate::num::atoi::is_digit;

/// The parts of a decimal literal, before any rounding decision.
struct Decimal {
    /// Up to 19 significant digits, as an integer.
    mantissa: u64,
    /// Power of ten to apply to `mantissa`.
    exp10: i64,
    negative: bool,
    /// Set when significant digits were dropped, which makes `mantissa` a
    /// lower bound rather than the exact value.
    truncated: bool,
}

/// Per-width constants for [`compute_float`].
pub(crate) trait RawFloat: Copy {
    const MANTISSA_EXPLICIT_BITS: i32;
    const MINIMUM_EXPONENT: i32;
    const INFINITE_POWER: i32;
    const SMALLEST_POWER_OF_TEN: i32;
    const LARGEST_POWER_OF_TEN: i32;
    const MIN_EXPONENT_ROUND_TO_EVEN: i32;
    const MAX_EXPONENT_ROUND_TO_EVEN: i32;
    /// Largest power of ten that is exactly representable.
    const MAX_EXACT_POW10: i32;
    /// Largest integer mantissa that is exactly representable.
    const MAX_EXACT_MANTISSA: u64;

    fn from_bits(mantissa: u64, exponent: i32) -> Self;
    fn from_u64_exact(v: u64) -> Self;
    fn pow10_exact(i: usize) -> Self;
    fn mul(self, rhs: Self) -> Self;
    fn div(self, rhs: Self) -> Self;
    fn neg(self) -> Self;
    fn parse_fallback(s: &str) -> Self;
}

const F64_POW10: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];

const F32_POW10: [f32; 11] = [1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10];

impl RawFloat for f64 {
    const MANTISSA_EXPLICIT_BITS: i32 = 52;
    const MINIMUM_EXPONENT: i32 = -1023;
    const INFINITE_POWER: i32 = 0x7FF;
    const SMALLEST_POWER_OF_TEN: i32 = -342;
    const LARGEST_POWER_OF_TEN: i32 = 308;
    const MIN_EXPONENT_ROUND_TO_EVEN: i32 = -4;
    const MAX_EXPONENT_ROUND_TO_EVEN: i32 = 23;
    const MAX_EXACT_POW10: i32 = 22;
    const MAX_EXACT_MANTISSA: u64 = 1 << 53;

    #[inline(always)]
    fn from_bits(mantissa: u64, exponent: i32) -> Self {
        f64::from_bits(mantissa | ((exponent as u64) << 52))
    }
    #[inline(always)]
    fn from_u64_exact(v: u64) -> Self {
        v as f64
    }
    #[inline(always)]
    fn pow10_exact(i: usize) -> Self {
        F64_POW10[i]
    }
    #[inline(always)]
    fn mul(self, rhs: Self) -> Self {
        self * rhs
    }
    #[inline(always)]
    fn div(self, rhs: Self) -> Self {
        self / rhs
    }
    #[inline(always)]
    fn neg(self) -> Self {
        -self
    }
    #[inline(never)]
    fn parse_fallback(s: &str) -> Self {
        // The scanner accepts a strict subset of Rust's float grammar, so this
        // cannot fail. Panicking beats returning a silently wrong number from
        // the path that exists precisely because the other two can be wrong.
        s.parse::<f64>()
            .expect("scanner accepts only valid float syntax")
    }
}

impl RawFloat for f32 {
    const MANTISSA_EXPLICIT_BITS: i32 = 23;
    const MINIMUM_EXPONENT: i32 = -127;
    const INFINITE_POWER: i32 = 0xFF;
    const SMALLEST_POWER_OF_TEN: i32 = -65;
    const LARGEST_POWER_OF_TEN: i32 = 38;
    const MIN_EXPONENT_ROUND_TO_EVEN: i32 = -17;
    const MAX_EXPONENT_ROUND_TO_EVEN: i32 = 10;
    const MAX_EXACT_POW10: i32 = 10;
    const MAX_EXACT_MANTISSA: u64 = 1 << 24;

    #[inline(always)]
    fn from_bits(mantissa: u64, exponent: i32) -> Self {
        f32::from_bits((mantissa as u32) | ((exponent as u32) << 23))
    }
    #[inline(always)]
    fn from_u64_exact(v: u64) -> Self {
        v as f32
    }
    #[inline(always)]
    fn pow10_exact(i: usize) -> Self {
        F32_POW10[i]
    }
    #[inline(always)]
    fn mul(self, rhs: Self) -> Self {
        self * rhs
    }
    #[inline(always)]
    fn div(self, rhs: Self) -> Self {
        self / rhs
    }
    #[inline(always)]
    fn neg(self) -> Self {
        -self
    }
    #[inline(never)]
    fn parse_fallback(s: &str) -> Self {
        // The scanner accepts a strict subset of Rust's float grammar, so this
        // cannot fail. Panicking beats returning a silently wrong number from
        // the path that exists precisely because the other two can be wrong.
        s.parse::<f32>()
            .expect("scanner accepts only valid float syntax")
    }
}

/// Binary exponent of `10^q`, exact for the range the table covers.
/// `217706 / 65536` approximates `log2(10)`.
#[inline(always)]
const fn power(q: i32) -> i32 {
    ((q.wrapping_mul(217_706)) >> 16) + 63
}

#[inline(always)]
const fn full_mul(a: u64, b: u64) -> (u64, u64) {
    let r = (a as u128) * (b as u128);
    (r as u64, (r >> 64) as u64)
}

/// Top 128 bits of `w * 5^q`, or close enough to decide the rounding.
///
/// The 192-bit product is `(w * hi) << 64 + (w * lo)`; only the leading 128
/// bits matter. The second multiply is skipped unless the low bits of the
/// leading word sit right on a rounding boundary.
#[inline(always)]
fn product_approx(q: i64, w: u64, precision: i32) -> (u64, u64) {
    let mask: u64 = if precision < 64 {
        u64::MAX >> precision
    } else {
        u64::MAX
    };
    debug_assert!(q >= SMALLEST_POWER_OF_FIVE as i64 && q <= LARGEST_POWER_OF_FIVE as i64);
    let index = (q - SMALLEST_POWER_OF_FIVE as i64) as usize;
    let (blo, bhi) = POWER_OF_FIVE_128[index];

    let (mut lo, mut hi) = full_mul(w, bhi);
    if hi & mask == mask {
        let (_, second_hi) = full_mul(w, blo);
        lo = lo.wrapping_add(second_hi);
        if second_hi > lo {
            hi += 1;
        }
    }
    (lo, hi)
}

/// Biased mantissa and exponent, or `None` when 128 bits could not decide.
fn compute_float<F: RawFloat>(q: i64, mut w: u64) -> Option<(u64, i32)> {
    if w == 0 || q < F::SMALLEST_POWER_OF_TEN as i64 {
        return Some((0, 0));
    }
    if q > F::LARGEST_POWER_OF_TEN as i64 {
        return Some((0, F::INFINITE_POWER));
    }

    let lz = w.leading_zeros() as i32;
    w <<= lz;

    let (lo, hi) = product_approx(q, w, F::MANTISSA_EXPLICIT_BITS + 3);
    if lo == u64::MAX {
        // The approximation is saturated, so adding one could carry across the
        // rounding boundary. Outside this exponent window that cannot happen,
        // because the product is exact.
        let inside_safe_exponent = (-27..=55).contains(&q);
        if !inside_safe_exponent {
            return None;
        }
    }

    let upperbit = (hi >> 63) as i32;
    let shift = upperbit + 64 - F::MANTISSA_EXPLICIT_BITS - 3;
    let mut mantissa = hi >> shift;
    let mut power2 = power(q as i32) + upperbit - lz - F::MINIMUM_EXPONENT;

    if power2 <= 0 {
        if -power2 + 1 >= 64 {
            // More than 64 bits below the smallest subnormal.
            return Some((0, 0));
        }
        mantissa >>= -power2 + 1;
        mantissa += mantissa & 1;
        mantissa >>= 1;
        // Rounding up out of the subnormal range lands on the smallest normal,
        // which the exponent field encodes as 1.
        let e = (mantissa >= (1u64 << F::MANTISSA_EXPLICIT_BITS)) as i32;
        return Some((mantissa, e));
    }

    // A tie with an even mantissa rounds down, not up. This can only be reached
    // when the product was exact, which is why the exponent window is checked.
    if lo <= 1
        && q >= F::MIN_EXPONENT_ROUND_TO_EVEN as i64
        && q <= F::MAX_EXPONENT_ROUND_TO_EVEN as i64
        && mantissa & 3 == 1
        && (mantissa << shift) == hi
    {
        mantissa &= !1u64;
    }

    mantissa += mantissa & 1;
    mantissa >>= 1;
    if mantissa >= (2u64 << F::MANTISSA_EXPLICIT_BITS) {
        mantissa = 1u64 << F::MANTISSA_EXPLICIT_BITS;
        power2 += 1;
    }
    mantissa &= !(1u64 << F::MANTISSA_EXPLICIT_BITS);
    if power2 >= F::INFINITE_POWER {
        return Some((0, F::INFINITE_POWER));
    }
    Some((mantissa, power2))
}

/// Scan a JSON number literal. Leaves `*i` on the first byte after the token.
fn scan(buf: &[u8], i: &mut usize) -> PResult<Decimal> {
    let n = buf.len();
    let mut idx = *i;

    let negative = idx < n && buf[idx] == b'-';
    if negative {
        idx += 1;
    }

    let mut mantissa: u64 = 0;
    let mut digits = 0i32;
    let mut exp10: i64 = 0;
    let mut truncated = false;

    if idx >= n || !is_digit(buf[idx]) {
        return Err(ErrorCode::ExpectedNumber);
    }

    // Integer part. JSON forbids a leading zero followed by more digits.
    if buf[idx] == b'0' {
        idx += 1;
        if idx < n && is_digit(buf[idx]) {
            return Err(ErrorCode::InvalidNumber);
        }
    } else {
        while idx < n && is_digit(buf[idx]) {
            let d = buf[idx] - b'0';
            if digits < 19 {
                mantissa = mantissa * 10 + d as u64;
                digits += 1;
            } else {
                // Past 19 digits the value no longer fits; keep the magnitude
                // by growing the exponent instead.
                exp10 += 1;
                truncated |= d != 0;
            }
            idx += 1;
        }
    }

    // Fraction.
    if idx < n && buf[idx] == b'.' {
        idx += 1;
        if idx >= n || !is_digit(buf[idx]) {
            return Err(ErrorCode::InvalidNumber);
        }
        while idx < n && is_digit(buf[idx]) {
            let d = buf[idx] - b'0';
            if mantissa == 0 && d == 0 {
                // A leading zero after the point shifts the exponent without
                // contributing a significant digit.
                exp10 -= 1;
            } else if digits < 19 {
                mantissa = mantissa * 10 + d as u64;
                digits += 1;
                exp10 -= 1;
            } else {
                truncated |= d != 0;
            }
            idx += 1;
        }
    }

    // Exponent.
    if idx < n && (buf[idx] | 0x20) == b'e' {
        idx += 1;
        let mut exp_neg = false;
        if idx < n && (buf[idx] == b'+' || buf[idx] == b'-') {
            exp_neg = buf[idx] == b'-';
            idx += 1;
        }
        if idx >= n || !is_digit(buf[idx]) {
            return Err(ErrorCode::InvalidNumber);
        }
        let mut e: i64 = 0;
        while idx < n && is_digit(buf[idx]) {
            // Saturate rather than overflow; anything past this is already
            // zero or infinity.
            if e < 0x10_0000 {
                e = e * 10 + (buf[idx] - b'0') as i64;
            }
            idx += 1;
        }
        exp10 += if exp_neg { -e } else { e };
    }

    *i = idx;
    Ok(Decimal {
        mantissa,
        exp10,
        negative,
        truncated,
    })
}

/// Parse a JSON number into `F`, advancing `*i` past the token.
#[inline]
pub(crate) fn parse_float<F: RawFloat>(buf: &[u8], i: &mut usize) -> PResult<F> {
    let start = *i;
    let d = scan(buf, i)?;
    let end = *i;

    if d.mantissa == 0 {
        // Preserve the sign of zero: `-0.0` is a distinct value.
        let z = F::from_bits(0, 0);
        return Ok(if d.negative { z.neg() } else { z });
    }

    // Tier 1: both operands exactly representable, so one operation is exact.
    if !d.truncated
        && d.mantissa <= F::MAX_EXACT_MANTISSA
        && d.exp10 >= -(F::MAX_EXACT_POW10 as i64)
        && d.exp10 <= F::MAX_EXACT_POW10 as i64
    {
        let m = F::from_u64_exact(d.mantissa);
        let v = if d.exp10 < 0 {
            m.div(F::pow10_exact((-d.exp10) as usize))
        } else {
            m.mul(F::pow10_exact(d.exp10 as usize))
        };
        return Ok(if d.negative { v.neg() } else { v });
    }

    // Tier 2: Eisel-Lemire. When digits were dropped the true mantissa lies in
    // `[m, m+1]`, so both ends must round the same way for the result to be
    // certain.
    let resolved = match compute_float::<F>(d.exp10, d.mantissa) {
        Some(a) => {
            if d.truncated {
                match compute_float::<F>(d.exp10, d.mantissa + 1) {
                    Some(b) if a == b => Some(a),
                    _ => None,
                }
            } else {
                Some(a)
            }
        }
        None => None,
    };

    if let Some((mantissa, exponent)) = resolved {
        let v = F::from_bits(mantissa, exponent);
        return Ok(if d.negative { v.neg() } else { v });
    }

    // Tier 3: exact big-integer arithmetic, via the standard library. This is
    // reached for well under 0.1% of inputs even when they are chosen to be
    // hostile, so the UTF-8 check costs nothing measurable and buys the float
    // path its way out of `unsafe` entirely.
    let text = core::str::from_utf8(&buf[start..end]).map_err(|_| ErrorCode::InvalidNumber)?;
    Ok(F::parse_fallback(text))
}
