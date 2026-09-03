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
use crate::num::atoi::{SAFE_DIGITS, fold_digits, is_digit};

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
    /// Apply a sign to a non-negative value.
    ///
    /// Setting the sign bit rather than negating, so that a document whose
    /// signs are unpredictable does not pay a mispredicted branch per value.
    fn with_sign(self, negative: bool) -> Self;
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
    fn with_sign(self, negative: bool) -> Self {
        debug_assert!(!self.is_sign_negative());
        f64::from_bits(self.to_bits() | ((negative as u64) << 63))
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
    fn with_sign(self, negative: bool) -> Self {
        debug_assert!(!self.is_sign_negative());
        f32::from_bits(self.to_bits() | ((negative as u32) << 31))
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
///
/// Always inlined, for the reason `Parser::read_u64` gives: this is what an
/// array of floats runs per element, and left as a call it costs the parser
/// its cursor, spilled around the call and reloaded after it.
#[inline(always)]
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
///
/// The digits are accumulated blind and counted afterwards, which keeps the
/// per-digit work to the fold itself. A leading zero on the integer part, an
/// empty fraction, and a count past [`SAFE_DIGITS`] are all settled from the
/// positions the folds stopped at.
#[inline(always)]
fn scan(buf: &[u8], i: &mut usize) -> PResult<Decimal> {
    let n = buf.len();
    let mut idx = *i;

    let negative = idx < n && buf[idx] == b'-';
    idx += negative as usize;

    if idx >= n || !is_digit(buf[idx]) {
        return Err(ErrorCode::ExpectedNumber);
    }

    // Integer part. JSON forbids a leading zero followed by more digits.
    let int_start = idx;
    let (mut mantissa, mut idx) = fold_digits(buf, idx, 0);
    let int_digits = idx - int_start;
    if buf[int_start] == b'0' && int_digits > 1 {
        return Err(ErrorCode::InvalidNumber);
    }

    // Fraction.
    let mut frac_start = idx;
    let mut frac_digits = 0usize;
    if idx < n && buf[idx] == b'.' {
        frac_start = idx + 1;
        (mantissa, idx) = fold_digits(buf, frac_start, mantissa);
        frac_digits = idx - frac_start;
        if frac_digits == 0 {
            return Err(ErrorCode::InvalidNumber);
        }
    }
    let mut exp10 = -(frac_digits as i64);

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
    let mut truncated = false;
    if int_digits + frac_digits > SAFE_DIGITS {
        (mantissa, exp10, truncated) = refold_long(
            &buf[int_start..int_start + int_digits],
            &buf[frac_start..frac_start + frac_digits],
            exp10,
        );
    }
    Ok(Decimal {
        mantissa,
        exp10,
        negative,
        truncated,
    })
}

/// Redo a number of more than [`SAFE_DIGITS`] digits, whose blind accumulation
/// may have wrapped.
///
/// Leading zeros carry no value, so they are stepped over first; a number
/// that is short enough once they are gone was accumulated correctly and only
/// needs the exponent it already has. Otherwise the first [`SAFE_DIGITS`]
/// significant digits are folded again, every digit dropped after them
/// raises `exp10` by one, and `truncated` reports whether any of them was
/// nonzero, which is what tells the caller the mantissa is a lower bound.
///
/// Cold and out of line: a number this wide is past what an `f64` can
/// distinguish, and keeping it out of [`scan`] is what keeps that small.
/// The two runs of digits arrive as slices of the document, so nothing in
/// the caller has its address taken, which would pin it to the stack for the
/// whole of the array loop around it.
#[cold]
#[inline(never)]
fn refold_long(int: &[u8], frac: &[u8], exp10: i64) -> (u64, i64, bool) {
    // The two runs of digits, in order of significance, as one sequence.
    let digits = int
        .iter()
        .chain(frac)
        .map(|b| b - b'0')
        .skip_while(|&d| d == 0);

    let mut mantissa: u64 = 0;
    let mut kept = 0usize;
    let mut dropped = 0i64;
    let mut truncated = false;
    for d in digits {
        if kept < SAFE_DIGITS {
            mantissa = mantissa * 10 + d as u64;
            kept += 1;
        } else {
            dropped += 1;
            truncated |= d != 0;
        }
    }
    (mantissa, exp10 + dropped, truncated)
}

/// [`compute_float`] for a mantissa that had digits dropped from it.
///
/// The true mantissa then lies in `[m, m+1]`, so both ends must round the
/// same way for the result to be certain. Out of line and cold: a number
/// with more than nineteen significant digits is rare in any document, and
/// keeping the second product out of [`parse_float`] is what keeps the first
/// one inlined.
#[cold]
#[inline(never)]
fn compute_float_truncated<F: RawFloat>(q: i64, w: u64) -> Option<(u64, i32)> {
    match (compute_float::<F>(q, w), compute_float::<F>(q, w + 1)) {
        (Some(a), Some(b)) if a == b => Some(a),
        _ => None,
    }
}

/// Parse a JSON number into `F`, advancing `*i` past the token.
///
/// Always inlined: this is the body of an array of floats, and the parser's
/// `read_f64` says why a hint is not enough.
#[inline(always)]
pub(crate) fn parse_float<F: RawFloat>(buf: &[u8], i: &mut usize) -> PResult<F> {
    let start = *i;
    let d = scan(buf, i)?;
    let end = *i;

    if d.mantissa == 0 {
        // Preserve the sign of zero: `-0.0` is a distinct value.
        return Ok(F::from_bits(0, 0).with_sign(d.negative));
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
        return Ok(v.with_sign(d.negative));
    }

    // Tier 2: Eisel-Lemire.
    let resolved = if d.truncated {
        compute_float_truncated::<F>(d.exp10, d.mantissa)
    } else {
        compute_float::<F>(d.exp10, d.mantissa)
    };

    if let Some((mantissa, exponent)) = resolved {
        return Ok(F::from_bits(mantissa, exponent).with_sign(d.negative));
    }

    // Tier 3: exact big-integer arithmetic, via the standard library. This is
    // reached for well under 0.1% of inputs even when they are chosen to be
    // hostile, so the UTF-8 check costs nothing measurable and buys the float
    // path its way out of `unsafe` entirely.
    let text = core::str::from_utf8(&buf[start..end]).map_err(|_| ErrorCode::InvalidNumber)?;
    Ok(F::parse_fallback(text))
}

/// Walk a JSON number literal without converting it, leaving `*i` on the first
/// byte after the token.
///
/// The same grammar [`parse_float`] holds its input to, because it is the same
/// walk: [`Parser::read_number_str`](crate::json::Parser::read_number_str)
/// hands back the digits for a type this crate cannot convert to, and a token
/// it accepted must be one every other reader would have accepted too.
#[inline]
pub(crate) fn scan_number(buf: &[u8], i: &mut usize) -> PResult<()> {
    scan(buf, i)?;
    Ok(())
}

/// Whether `s` is one JSON number literal and nothing else.
///
/// The check behind
/// [`Writer::write_number_str`](crate::json::Writer::write_number_str), which
/// only runs it under `debug_assertions`.
pub(crate) fn is_number(s: &str) -> bool {
    let mut i = 0;
    scan_number(s.as_bytes(), &mut i).is_ok() && i == s.len()
}
