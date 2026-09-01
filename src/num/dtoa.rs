//! Float serialization: shortest round-trip digits, formatted the way Glaze
//! formats them.
//!
//! The digits come from [`super::zmij`]; this module decides how to spell them.
//!
//! Output policy, matched against Glaze's `zmij::to_chars`:
//!
//! - Shortest decimal that round-trips back to the same bits.
//! - Fixed notation when the scientific exponent is in `[-4, MAX_FIXED]`,
//!   scientific otherwise. `MAX_FIXED` is 15 for `f64` and 6 for `f32`, which
//!   is `floor(mantissa_bits * log10(2))`, the same count C exposes as
//!   `DBL_DIG` and `FLT_DIG`.
//! - No trailing `.0`: an integral value writes as `5`, not `5.0`.
//! - Uppercase `E`, no `+` on positive exponents, no leading zero (`E7`, not
//!   `E07`).
//! - `-0.0` writes as `-0`.
//!
//! Non-finite values have no JSON representation. [`write_f64`] reports that to
//! the caller rather than inventing one.

use super::zmij::{DIGIT_SCRATCH, Digits, digits_f32, digits_f64};

/// Output buffer width.
///
/// The longest real output is a sign, 17 digits, a point, `E`, a sign and three
/// exponent digits. The rest is headroom so the fixed-width block copies in
/// [`render`] never need a bounds-dependent length.
pub(crate) const MAX_FLOAT_BYTES: usize = 48;

/// Width of the block copies below. Larger than the 17 digits a `f64` can
/// need, so one block always covers a whole run.
const BLOCK: usize = 24;

/// The scientific exponents still written in fixed notation. Below the
/// minimum or above the maximum, the output goes scientific.
const MIN_FIXED: i32 = -4;
const MAX_FIXED_F64: i32 = 15;
const MAX_FIXED_F32: i32 = 6;

/// Copy a `BLOCK`-sized run. The length is a constant, so this lowers to a
/// couple of wide stores instead of a `memcpy` call with a runtime length,
/// which is what the digit assembly used to spend most of its time on.
#[inline(always)]
fn copy_block(src: &[u8; DIGIT_SCRATCH], from: usize, dst: &mut [u8; MAX_FLOAT_BYTES], to: usize) {
    debug_assert!(from + BLOCK <= DIGIT_SCRATCH && to + BLOCK <= MAX_FLOAT_BYTES);
    let block: [u8; BLOCK] = src[from..from + BLOCK].try_into().unwrap();
    dst[to..to + BLOCK].copy_from_slice(&block);
}

/// Render `d` into `out`, returning the number of bytes written.
fn render(negative: bool, d: &Digits, max_fixed: i32, out: &mut [u8; MAX_FLOAT_BYTES]) -> usize {
    // The sign is written unconditionally and then stepped over only when it
    // belongs, which keeps a branch out of every call.
    out[0] = b'-';
    let mut p = negative as usize;

    let nd = d.len;
    let start = d.start;

    if d.e10 >= MIN_FIXED && d.e10 <= max_fixed {
        if d.e10 >= 0 {
            let int_digits = d.e10 as usize + 1;
            copy_block(&d.buf, start, out, p);
            if int_digits >= nd {
                // Every significant digit is integral; pad with zeros.
                for i in nd..int_digits {
                    out[p + i] = b'0';
                }
                p += int_digits;
            } else {
                // The point falls inside the digits, so the run splits in two.
                p += int_digits;
                out[p] = b'.';
                p += 1;
                copy_block(&d.buf, start + int_digits, out, p);
                p += nd - int_digits;
            }
        } else {
            out[p] = b'0';
            out[p + 1] = b'.';
            p += 2;
            let zeros = (-d.e10 - 1) as usize;
            for i in 0..zeros {
                out[p + i] = b'0';
            }
            p += zeros;
            copy_block(&d.buf, start, out, p);
            p += nd;
        }
    } else {
        // Scientific: d[.ddd]E[-]dd
        out[p] = d.buf[start];
        p += 1;
        if nd > 1 {
            out[p] = b'.';
            p += 1;
            copy_block(&d.buf, start + 1, out, p);
            p += nd - 1;
        }
        out[p] = b'E';
        p += 1;
        let mut e = d.e10;
        if e < 0 {
            out[p] = b'-';
            p += 1;
            e = -e;
        }
        // At most three digits, and never a leading zero: `E7`, not `E07`.
        if e >= 100 {
            out[p] = b'0' + (e / 100) as u8;
            p += 1;
        }
        if e >= 10 {
            out[p] = b'0' + ((e / 10) % 10) as u8;
            p += 1;
        }
        out[p] = b'0' + (e % 10) as u8;
        p += 1;
    }
    p
}

/// Write a signed zero, which has no digits to generate.
#[inline]
fn write_zero(negative: bool, out: &mut [u8; MAX_FLOAT_BYTES]) -> usize {
    out[0] = b'-';
    out[negative as usize] = b'0';
    negative as usize + 1
}

/// Serialize an `f64`. Returns `None` for NaN and infinity, which JSON cannot
/// represent.
#[inline]
pub(crate) fn write_f64(v: f64, out: &mut [u8; MAX_FLOAT_BYTES]) -> Option<usize> {
    if !v.is_finite() {
        return None;
    }
    let negative = v.is_sign_negative();
    let mag = if negative { -v } else { v };
    if mag == 0.0 {
        return Some(write_zero(negative, out));
    }
    Some(render(negative, &digits_f64(mag), MAX_FIXED_F64, out))
}

/// Serialize an `f32`. Returns `None` for NaN and infinity.
#[inline]
pub(crate) fn write_f32(v: f32, out: &mut [u8; MAX_FLOAT_BYTES]) -> Option<usize> {
    if !v.is_finite() {
        return None;
    }
    let negative = v.is_sign_negative();
    let mag = if negative { -v } else { v };
    if mag == 0.0 {
        return Some(write_zero(negative, out));
    }
    Some(render(negative, &digits_f32(mag), MAX_FIXED_F32, out))
}
