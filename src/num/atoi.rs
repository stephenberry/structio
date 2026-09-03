//! Integer parsing.
//!
//! The digit loop is SWAR: eight bytes are loaded at once, validated as digits
//! with two adds and a mask, and folded into a value with three multiplies.
//! This is the same shape as Glaze's `atoi.hpp`.

use super::ZEROS;
use crate::error::{ErrorCode, PResult};
use crate::swar::{first_match, load_u64};

/// Light the high bit of every lane of this little-endian word that is not
/// an ASCII digit.
///
/// `b` goes negative in the high bit of any lane below `'0'`; `a` carries into
/// the high bit of any lane above `'9'`. A digit lane does neither, and it
/// neither carries nor borrows into the lane above it, so the lanes below the
/// first non-digit are all clear and that lane is genuinely lit. A lit lane
/// above it may be a carry out of a real match rather than a match of its
/// own, which is the same rule the string masks in `swar` live by: the lowest
/// set bit is exact, and it is the only bit a caller may read.
#[inline(always)]
pub(crate) const fn digit_stop_mask(v: u64) -> u64 {
    let a = v.wrapping_add(0x4646_4646_4646_4646);
    let b = v.wrapping_sub(ZEROS);
    (a | b) & 0x8080_8080_8080_8080
}

/// Fold eight ASCII digits into the integer they spell.
///
/// Three rounds of pairwise combination: digits into 2-digit groups, those into
/// 4-digit groups, those into the final value.
#[inline(always)]
pub(crate) const fn parse_8_digits(v: u64) -> u64 {
    const MASK: u64 = 0x0000_00FF_0000_00FF;
    const MUL1: u64 = 0x000F_4240_0000_0064; // 10^6 and 10^2
    const MUL2: u64 = 0x0000_2710_0000_0001; // 10^4 and 10^0
    let mut val = v - ZEROS;
    val = (val * 10) + (val >> 8);
    (((val & MASK).wrapping_mul(MUL1)) + (((val >> 16) & MASK).wrapping_mul(MUL2))) >> 32
}

#[inline(always)]
pub(crate) const fn is_digit(c: u8) -> bool {
    c.wrapping_sub(b'0') < 10
}

/// The most digits that always fit a `u64`, since `10^19 - 1 < u64::MAX`.
/// Stopping here is what lets [`fold_digits`] carry no overflow check: its
/// callers count the digits afterwards and redo any longer run on a path
/// that can afford to.
pub(crate) const SAFE_DIGITS: usize = 19;

/// `10^k` for `k` in `0..=8`: the scale a run of `k` digits applies to what
/// came before it.
pub(crate) const POW10_U64: [u64; 9] = [
    1,
    10,
    100,
    1_000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
];

/// The value of the first `k` bytes of `word` as digits, for `k` in `0..=7`.
///
/// The digits are shifted up to the top of the word and the bytes they vacate
/// filled with `'0'`, so an eight-digit fold reads them as a number with
/// leading zeros. A number of any length under eight therefore costs the
/// same as one of exactly eight, with no loop over its bytes and no branch
/// on how many there were. The loop it replaces exited at a different digit
/// on every value of a document whose numbers vary in length, and that exit
/// mispredicted about as often as it ran.
#[inline(always)]
pub(crate) fn parse_leading_digits(word: u64, k: usize) -> u64 {
    debug_assert!(k <= 7);
    // Both shifts are in `8..=56`; a `k` of zero shares the shift for one,
    // and its word is replaced by all zeros before the fold, so the
    // terminator in the top byte never reaches the digit arithmetic.
    let s = 8 * (8 - k.max(1));
    let filled = (word << s) | (ZEROS >> (64 - s));
    parse_8_digits(core::hint::select_unpredictable(k == 0, ZEROS, filled))
}

/// Parse an unsigned decimal at `*i`, stopping at the first non-digit.
///
/// Returns the value and leaves `*i` on the terminator. Rejects a leading zero
/// followed by another digit, which JSON does not allow.
///
/// This is deliberately small enough to inline into a caller's loop, because
/// the caller is usually reading an array and a call per element costs more
/// than the digits do: it spills the parser's cursor to the stack and reloads
/// it on every return. The common case is one word: up to seven digits and
/// the byte that ends them, folded in one step by [`parse_leading_digits`].
/// A second word takes a number up to fifteen digits, which is where
/// identifiers and millisecond timestamps live, by the same step again.
/// Everything else, sixteen digits or more and a number within fifteen bytes
/// of the end of the document, lives in [`parse_u64_wide`], out of line, so
/// that the size of the rare cases cannot price the common one out of being
/// inlined.
#[inline(always)]
pub(crate) fn parse_u64(buf: &[u8], i: &mut usize) -> PResult<u64> {
    let idx = *i;
    if idx + 16 <= buf.len() {
        // SAFETY: `idx + 16 <= buf.len()` covers both loads.
        let (first_word, second_word) = unsafe { (load_u64(buf, idx), load_u64(buf, idx + 8)) };
        let first = first_word as u8;
        let stop = digit_stop_mask(first_word);
        if stop != 0 {
            // The lowest lit lane is the first byte that is not a digit.
            let k = first_match(stop);
            if k == 0 {
                return Err(not_a_number(first));
            }
            // JSON forbids leading zeros: `0` alone is fine, `01` is not.
            if first == b'0' && k > 1 {
                return Err(ErrorCode::InvalidNumber);
            }
            *i = idx + k;
            return Ok(parse_leading_digits(first_word, k));
        }
        // Eight digits at least, so a zero in front is a leading zero.
        if first == b'0' {
            return Err(ErrorCode::InvalidNumber);
        }
        let stop = digit_stop_mask(second_word);
        if stop != 0 {
            let k = first_match(stop);
            *i = idx + 8 + k;
            // Fifteen digits at most, so nothing here can overflow.
            return Ok(
                parse_8_digits(first_word) * POW10_U64[k] + parse_leading_digits(second_word, k)
            );
        }
    }
    let (value, end) = parse_u64_wide(buf, idx)?;
    *i = end;
    Ok(value)
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

/// Accumulate the run of ASCII digits at `idx` onto `acc`, wrapping, and
/// return the value with the index of the first byte that is not a digit.
///
/// A word at a time while a whole word is readable. A word of eight digits
/// folds in one step, and so does the word holding the end of the run, by
/// [`parse_leading_digits`], so a run of any length under eight costs the
/// same as one of exactly eight and there is no loop over its bytes. The
/// byte loop only runs for a number sitting in the last seven bytes of the
/// document.
///
/// Wrapping is deliberate: the caller counts the digits and redoes any run
/// past [`SAFE_DIGITS`] on a path that can afford to, so the value here only
/// has to be right when the count says it is. Everything crosses by value so
/// that an inlining caller keeps its cursor in a register.
#[inline(always)]
pub(crate) fn fold_digits(buf: &[u8], mut idx: usize, mut acc: u64) -> (u64, usize) {
    let n = buf.len();

    while idx + 8 <= n {
        // SAFETY: the loop condition is `idx + 8 <= n`.
        let word = unsafe { load_u64(buf, idx) };
        let stop = digit_stop_mask(word);
        if stop != 0 {
            // The lowest lit lane is the first byte that is not a digit.
            let k = first_match(stop);
            acc = acc
                .wrapping_mul(POW10_U64[k])
                .wrapping_add(parse_leading_digits(word, k));
            return (acc, idx + k);
        }
        acc = acc
            .wrapping_mul(100_000_000)
            .wrapping_add(parse_8_digits(word));
        idx += 8;
    }

    while idx < n {
        let d = buf[idx].wrapping_sub(b'0');
        if d >= 10 {
            break;
        }
        acc = acc.wrapping_mul(10).wrapping_add(d as u64);
        idx += 1;
    }
    (acc, idx)
}

/// Numbers [`parse_u64`] hands off: sixteen digits or more, and numbers that
/// start within fifteen bytes of the end of the buffer.
///
/// Takes the position of the first byte and returns the value with the
/// position after it. By value in both directions, because a `&mut` to the
/// caller's cursor would make the cursor addressable, and an addressable
/// cursor lives on the stack for the whole of the array loop around the
/// call rather than in a register.
///
/// Accumulation is unchecked and the overflow question is settled once at the
/// end, from the digit count, rather than with a branch per digit.
#[inline(never)]
fn parse_u64_wide(buf: &[u8], start: usize) -> PResult<(u64, usize)> {
    if start >= buf.len() {
        return Err(ErrorCode::UnexpectedEnd);
    }
    let first = buf[start];
    if !is_digit(first) {
        return Err(not_a_number(first));
    }
    let (value, end) = fold_digits(buf, start, 0);
    finish_wide(buf, start, end, value)
}

/// The checks [`parse_u64_wide`] settles once the run of digits is known:
/// a leading zero, and a count that may have wrapped the accumulation.
#[inline(always)]
fn finish_wide(buf: &[u8], start: usize, end: usize, value: u64) -> PResult<(u64, usize)> {
    let len = end - start;
    if buf[start] == b'0' && len > 1 {
        return Err(ErrorCode::InvalidNumber);
    }
    if len > SAFE_DIGITS {
        return recheck_wide(buf, start, end).map(|v| (v, end));
    }
    Ok((value, end))
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
/// A sign test and a range compare around [`parse_u64`], and inlined for the
/// same reason: this is what an array of signed integers calls per element,
/// and left out of line it puts the call back that `parse_u64` was shaped to
/// remove.
///
/// The sign is applied without a branch on it. A document whose signs are
/// unpredictable, which a column of measurements is, mispredicted a branch
/// here on about every other value, and that cost more than the digits.
#[inline(always)]
pub(crate) fn parse_i64(buf: &[u8], i: &mut usize) -> PResult<i64> {
    let negative = buf.get(*i) == Some(&b'-');
    *i += negative as usize;
    let magnitude = parse_u64(buf, i)?;
    // `-(2^63)` has no positive counterpart, so a negative value may reach
    // one past `i64::MAX`.
    if magnitude > (i64::MAX as u64) + negative as u64 {
        return Err(ErrorCode::NumberOutOfRange);
    }
    // Two's complement negation, flip and add one, applied through a mask
    // that is all ones or all zeros. At `2^63` the add wraps, onto `i64::MIN`,
    // which is the right answer.
    let flip = (negative as i64).wrapping_neg();
    Ok(((magnitude as i64) ^ flip).wrapping_sub(flip))
}
