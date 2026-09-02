//! Integer parsing.
//!
//! The digit loop is SWAR: eight bytes are loaded at once, validated as digits
//! with two adds and a mask, and folded into a value with three multiplies.
//! This is the same shape as Glaze's `atoi.hpp`.

use crate::error::{ErrorCode, PResult};
use crate::swar::load_u64;

/// Are all eight bytes of this little-endian word ASCII digits?
///
/// `b` goes negative in the high bit of any lane below `'0'`; `a` carries into
/// the high bit of any lane above `'9'`. Neither fires only when every lane is
/// in range.
#[inline(always)]
const fn is_8_digits(v: u64) -> bool {
    let a = v.wrapping_add(0x4646_4646_4646_4646);
    let b = v.wrapping_sub(0x3030_3030_3030_3030);
    (a | b) & 0x8080_8080_8080_8080 == 0
}

/// Fold eight ASCII digits into the integer they spell.
///
/// Three rounds of pairwise combination: digits into 2-digit groups, those into
/// 4-digit groups, those into the final value.
#[inline(always)]
const fn parse_8_digits(v: u64) -> u64 {
    const MASK: u64 = 0x0000_00FF_0000_00FF;
    const MUL1: u64 = 0x000F_4240_0000_0064; // 10^6 and 10^2
    const MUL2: u64 = 0x0000_2710_0000_0001; // 10^4 and 10^0
    let mut val = v - 0x3030_3030_3030_3030;
    val = (val * 10) + (val >> 8);
    (((val & MASK).wrapping_mul(MUL1)) + (((val >> 16) & MASK).wrapping_mul(MUL2))) >> 32
}

#[inline(always)]
pub(crate) const fn is_digit(c: u8) -> bool {
    c.wrapping_sub(b'0') < 10
}

/// The most digits that always fit a `u64`, since `10^19 - 1 < u64::MAX`.
/// Stopping here is what lets the accumulation below carry no overflow check.
const SAFE_DIGITS: usize = 19;

/// Parse an unsigned decimal at `*i`, stopping at the first non-digit.
///
/// Returns the value and leaves `*i` on the terminator. Rejects a leading zero
/// followed by another digit, which JSON does not allow.
///
/// This is deliberately small enough to inline into a caller's loop, because
/// the caller is usually reading an array and a call per element costs more
/// than the digits do: it spills the parser's cursor to the stack and reloads
/// it on every return. Everything a short number does not need lives in
/// [`parse_u64_wide`], out of line, so that the size of the rare cases cannot
/// price the common one out of being inlined.
#[inline(always)]
pub(crate) fn parse_u64(buf: &[u8], i: &mut usize) -> PResult<u64> {
    let n = buf.len();
    let idx = *i;

    if idx >= n {
        return Err(ErrorCode::UnexpectedEnd);
    }
    let first = buf[idx].wrapping_sub(b'0');
    if first >= 10 {
        return Err(not_a_number(buf[idx]));
    }

    // JSON forbids leading zeros: `0` alone is fine, `01` is not.
    if first == 0 {
        if idx + 1 < n && is_digit(buf[idx + 1]) {
            return Err(ErrorCode::InvalidNumber);
        }
        *i = idx + 1;
        return Ok(0);
    }

    // Up to nineteen digits, so no step can overflow and the loop carries no
    // check but the one that finds the end of the number.
    let stop = n.min(idx + SAFE_DIGITS);
    let mut value = first as u64;
    let mut k = idx + 1;
    while k < stop {
        let c = buf[k].wrapping_sub(b'0');
        if c >= 10 {
            *i = k;
            return Ok(value);
        }
        value = value * 10 + c as u64;
        k += 1;
    }

    // Either the number is wide enough that overflow is back in question, or
    // the buffer ended before a terminator did. Neither is the common case.
    parse_u64_wide(buf, i, idx)
}

/// The first byte where a number was expected is not a digit.
///
/// Out of line so that the error text does not count against the size of
/// [`parse_u64`], which is chosen to be inlinable.
#[cold]
#[inline(never)]
fn not_a_number(first: u8) -> ErrorCode {
    // A well formed negative number is still a number; it just cannot fit an
    // unsigned target, and saying so is more useful than "expected a number"
    // when the input reads `-1`.
    if first == b'-' {
        ErrorCode::NumberOutOfRange
    } else {
        ErrorCode::ExpectedNumber
    }
}

/// Numbers [`parse_u64`] hands off: nineteen digits or more, and numbers that
/// run to the end of the buffer.
///
/// `start` is the first digit, which the caller has already established is one
/// and is not a lone leading zero. Accumulation is unchecked and the overflow
/// question is settled once at the end, from the digit count, rather than with
/// a branch per digit.
#[inline(never)]
fn parse_u64_wide(buf: &[u8], i: &mut usize, start: usize) -> PResult<u64> {
    let n = buf.len();
    let mut idx = start;
    let mut value: u64 = 0;

    // Bulk: eight digits per iteration while a full word is readable and every
    // lane is a digit.
    while idx + 8 <= n {
        // SAFETY: the loop condition is `idx + 8 <= n`.
        let word = unsafe { load_u64(buf, idx) };
        if !is_8_digits(word) {
            break;
        }
        value = value
            .wrapping_mul(100_000_000)
            .wrapping_add(parse_8_digits(word));
        idx += 8;
    }

    // Tail: whatever the bulk loop stopped short of.
    while idx < n {
        let c = buf[idx].wrapping_sub(b'0');
        if c >= 10 {
            break;
        }
        value = value.wrapping_mul(10).wrapping_add(c as u64);
        idx += 1;
    }

    let len = idx - start;
    *i = idx;
    if len > SAFE_DIGITS {
        return recheck_wide(buf, start, idx);
    }
    Ok(value)
}

/// Twenty or more digits: the wrapping accumulation above may have overflowed,
/// so redo it with checked arithmetic.
///
/// Out of line and cold, because a `u64` that needs this many digits is at or
/// past the type's limit and is almost always an error in the document.
#[cold]
#[inline(never)]
fn recheck_wide(buf: &[u8], start: usize, end: usize) -> PResult<u64> {
    let mut value: u64 = 0;
    let mut idx = start;
    while idx < end {
        let c = buf[idx] - b'0';
        value = match value.checked_mul(10).and_then(|v| v.checked_add(c as u64)) {
            Some(v) => v,
            None => return Err(ErrorCode::NumberOutOfRange),
        };
        idx += 1;
    }
    Ok(value)
}

/// Reject a numeric token that continues into fractional or exponent syntax.
///
/// An integer target must not silently truncate `1.5` or misread `1e3`, so the
/// caller checks the terminator after the digits are consumed.
#[inline(always)]
pub(crate) fn reject_float_tail(buf: &[u8], i: usize) -> PResult<()> {
    if i < buf.len() {
        match buf[i] {
            b'.' | b'e' | b'E' => return Err(ErrorCode::InvalidNumber),
            _ => {}
        }
    }
    Ok(())
}

/// Parse a signed integer into `i64`, then let the caller narrow.
///
/// A sign test and two range compares around [`parse_u64`], and inlined for
/// the same reason: this is what an array of signed integers calls per
/// element, and left out of line it puts the call back that `parse_u64` was
/// shaped to remove.
#[inline(always)]
pub(crate) fn parse_i64(buf: &[u8], i: &mut usize) -> PResult<i64> {
    let negative = *i < buf.len() && buf[*i] == b'-';
    if negative {
        *i += 1;
    }
    let magnitude = parse_u64(buf, i)?;
    if negative {
        // `-(2^63)` has no positive counterpart, so compare before negating.
        if magnitude > (i64::MAX as u64) + 1 {
            return Err(ErrorCode::NumberOutOfRange);
        }
        Ok((magnitude as i64).wrapping_neg())
    } else {
        if magnitude > i64::MAX as u64 {
            return Err(ErrorCode::NumberOutOfRange);
        }
        Ok(magnitude as i64)
    }
}
