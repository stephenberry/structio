//! The two things every BEVE value is built from: a header byte and a
//! compressed size.
//!
//! Both are small enough that they are given as `const fn`s, which lets the
//! [`object!`](crate::object) macro assemble a struct's key encodings during
//! const evaluation and lets every header used by the impls fold to a literal.
//!
//! # Header layout
//!
//! ```text
//! bit  7 6 5   4 3   2 1 0
//!     [ count ][ sub ][ ty ]
//! ```
//!
//! `ty` is the value's kind, `sub` narrows it (a number's signedness, an
//! object's key type, an array's element category), and `count` is the
//! [byte-count code](byte_width): the element width is `1 << count`, with
//! floats the one documented exception.

use crate::error::{ErrorCode, PResult};

// --- Types (bits 0-2) ------------------------------------------------------

pub const TY_NULL_BOOL: u8 = 0;
pub const TY_NUMBER: u8 = 1;
pub const TY_STRING: u8 = 2;
pub const TY_OBJECT: u8 = 3;
pub const TY_TYPED_ARRAY: u8 = 4;
pub const TY_GENERIC_ARRAY: u8 = 5;
pub const TY_EXTENSION: u8 = 6;
/// The one type code the specification leaves undefined.
///
/// The field is three bits wide and six values are spoken for, so this is the
/// only bit pattern that can stand for something which is not a value at all.
/// [`complex_element`] is the one thing that needs such a code, and no document
/// may carry it: a header read out of the input with this type is refused.
pub const TY_UNDEFINED: u8 = 7;

// --- Sub-types (bits 3-4) --------------------------------------------------

/// Number and typed-array element categories, and object key types. The three
/// share an encoding, which is what lets a typed array's header become its
/// element's header by swapping the type bits alone.
pub const CAT_FLOAT: u8 = 0;
pub const CAT_SIGNED: u8 = 1;
pub const CAT_UNSIGNED: u8 = 2;
/// Typed arrays only: booleans, strings, or an aligned numeric block, told
/// apart by the byte-count field.
pub const CAT_OTHER: u8 = 3;

/// Byte-count values under [`CAT_OTHER`].
pub const OTHER_BOOL: u8 = 0;
pub const OTHER_STRING: u8 = 1;
pub const OTHER_ALIGNED: u8 = 2;

// --- Extension ids (bits 3-7) ----------------------------------------------

pub const EXT_DELIMITER: u8 = 0;
pub const EXT_TYPE_TAG: u8 = 1;
pub const EXT_MATRIX: u8 = 2;
pub const EXT_COMPLEX: u8 = 3;

// --- Whole headers ---------------------------------------------------------

/// Assemble a header from its three fields.
#[inline(always)]
pub const fn header(ty: u8, sub: u8, count: u8) -> u8 {
    (count << 5) | ((sub & 0b11) << 3) | (ty & 0b111)
}

pub const NULL: u8 = 0;
pub const FALSE: u8 = 0b0000_1000;
pub const TRUE: u8 = 0b0001_1000;
pub const STRING: u8 = TY_STRING;
pub const GENERIC_ARRAY: u8 = TY_GENERIC_ARRAY;
/// An object with string keys, which is what every `object!` struct is.
pub const OBJECT: u8 = TY_OBJECT;
/// The delimiter extension: a marker with no body, which the specification
/// offers for separating documents in a stream. Assembled by hand because an
/// extension carries its id in the five bits above the type rather than in the
/// `sub` and `count` fields [`header`] takes.
pub const DELIMITER: u8 = TY_EXTENSION | (EXT_DELIMITER << 3);

/// The matrix extension: a layout byte, then the extents and the data, each a
/// value of its own.
pub const MATRIX: u8 = TY_EXTENSION | (EXT_MATRIX << 3);

/// The complex extension, which a [class header](complex_class) always follows.
pub const COMPLEX: u8 = TY_EXTENSION | (EXT_COMPLEX << 3);

/// The number header for a `count`-coded value of `cat`.
pub const fn number(cat: u8, count: u8) -> u8 {
    header(TY_NUMBER, cat, count)
}

/// The typed-array header for elements of `cat` at width code `count`.
pub const fn array_of(cat: u8, count: u8) -> u8 {
    header(TY_TYPED_ARRAY, cat, count)
}

pub const BOOL_ARRAY: u8 = array_of(CAT_OTHER, OTHER_BOOL);
pub const STRING_ARRAY: u8 = array_of(CAT_OTHER, OTHER_STRING);
pub const ALIGNED_ARRAY: u8 = array_of(CAT_OTHER, OTHER_ALIGNED);

// --- Extension bodies -----------------------------------------------------

/// A matrix stored with its rightmost index varying fastest: row major.
pub const LAYOUT_RIGHT: u8 = 0;
/// A matrix stored with its leftmost index varying fastest: column major.
pub const LAYOUT_LEFT: u8 = 1;

/// A lone complex number: the class header is followed by one pair.
pub const COMPLEX_ONE: u8 = 0;
/// A run of complex numbers: a size stands between the class header and the
/// pairs.
pub const COMPLEX_MANY: u8 = 1;

/// The class header a complex value carries, in the byte after
/// [`COMPLEX`].
///
/// The class and byte-count fields sit exactly where [`number`] puts them, so
/// the width of a complex component is read by the same [`byte_width`]. What
/// differs is the low three bits: a number header spends them on its type,
/// this one on [`COMPLEX_ONE`] or [`COMPLEX_MANY`]. The field is three bits
/// wide for that alignment and no other reason, and the other six values are
/// undefined; a reader must refuse them rather than guess, because the two
/// defined forms differ by whether a size precedes the payload.
#[inline(always)]
pub const fn complex_class(cat: u8, count: u8, form: u8) -> u8 {
    header(form, cat, count)
}

/// The header a complex array's elements are matched against.
///
/// Synthetic, and by construction not a byte any document can hold. A complex
/// element carries no header at all, so something has to stand for one, and the
/// obvious candidate is unusable: a class header of form [`COMPLEX_MANY`] is
/// bit for bit the [`number`] header of the same class and width, `0x61` being
/// both a complex array of `f64` and a lone `f64`. This is [`element_of`] with
/// [`TY_UNDEFINED`] in place of the type, which keeps the class and the width
/// where every reader already looks for them and leaves a byte equal to nothing
/// else at all. So a `Vec<f64>` cannot bulk-read the payload of a complex
/// array, and nothing has to ask where a header came from to know what it is.
#[inline(always)]
pub const fn complex_element(class: u8) -> u8 {
    (class & 0b1111_1000) | TY_UNDEFINED
}

#[inline(always)]
pub const fn ty(h: u8) -> u8 {
    h & 0b111
}

#[inline(always)]
pub const fn sub(h: u8) -> u8 {
    (h >> 3) & 0b11
}

#[inline(always)]
pub const fn count(h: u8) -> u8 {
    h >> 5
}

#[inline(always)]
pub const fn ext_id(h: u8) -> u8 {
    h >> 3
}

/// Turn a typed array's header into the header its elements would carry if
/// they were written as standalone values.
///
/// The category and width fields already line up; only the type changes. For
/// [`CAT_OTHER`] arrays the result is meaningless and the caller must not ask.
#[inline(always)]
pub const fn element_of(array: u8) -> u8 {
    (array & 0b1111_1000) | TY_NUMBER
}

/// The byte-count code for a `width`-byte value.
///
/// The inverse of `1 << count`, which is the rule for every width this crate
/// writes. It is not used for the two 16-bit floats, whose codes do not follow
/// that rule; see [`byte_width`].
pub const fn code_for(width: usize) -> u8 {
    width.trailing_zeros() as u8
}

/// Bytes one element of category `cat` at width code `count` occupies.
///
/// The integer categories follow `1 << count` exactly. Floats do not: BEVE has
/// no 8-bit float, so code 0 is `bfloat16` and code 1 is `float16`, both two
/// bytes wide. Every width calculation goes through here so that exception
/// cannot be forgotten at one call site and honoured at another.
pub const fn byte_width(cat: u8, count: u8) -> Option<usize> {
    match cat {
        CAT_FLOAT => match count {
            0 | 1 => Some(2),
            2 => Some(4),
            3 => Some(8),
            4 => Some(16),
            _ => None,
        },
        CAT_SIGNED | CAT_UNSIGNED => {
            if count <= 4 {
                Some(1usize << count)
            } else {
                None
            }
        }
        _ => None,
    }
}

// --- Compressed unsigned integers ------------------------------------------

/// Largest size the codec can express: 2^62 - 1.
pub const MAX_SIZE: u64 = (1 << 62) - 1;

/// Bytes [`encode_size`] emits for `n`.
///
/// The thresholds live here alone, so a length computed ahead of time cannot
/// disagree with the bytes actually written.
#[inline]
pub const fn size_len(n: u64) -> usize {
    if n < (1 << 6) {
        1
    } else if n < (1 << 14) {
        2
    } else if n < (1 << 30) {
        4
    } else {
        8
    }
}

/// Encode `n` into `out`, returning the number of bytes used.
///
/// The low two bits of the first byte select the total width; the value fills
/// the remaining bits, little end first.
///
/// # Panics
///
/// If `n > MAX_SIZE`, which no size derived from a real collection can reach:
/// 2^62 elements do not fit in an address space.
#[inline]
pub const fn encode_size(n: u64, out: &mut [u8; 8]) -> usize {
    assert!(n <= MAX_SIZE, "structio: BEVE size out of range");
    let low = ((n as u8) & 0x3f) << 2;
    let rest = n >> 6;
    let width = size_len(n);
    out[0] = low | (width_code(width) as u8);
    let mut i = 1;
    while i < width {
        out[i] = (rest >> (8 * (i - 1))) as u8;
        i += 1;
    }
    width
}

const fn width_code(width: usize) -> usize {
    match width {
        1 => 0,
        2 => 1,
        4 => 2,
        _ => 3,
    }
}

/// Bytes of a compressed size that follow its first one.
///
/// The width lives in the low two bits, and this is the only place that reads
/// them: a stream has to decode a size a byte at a time rather than out of a
/// slice, and two copies of this table are two things that could disagree
/// about where a value ends.
#[inline(always)]
pub(crate) const fn size_extra(b0: u8) -> usize {
    match b0 & 0b11 {
        0 => 0,
        1 => 1,
        2 => 3,
        _ => 7,
    }
}

/// Decode a compressed size from `data` at `*pos`, advancing past it.
#[inline]
pub fn decode_size(data: &[u8], pos: &mut usize) -> PResult<u64> {
    let i = *pos;
    let &b0 = data.get(i).ok_or(ErrorCode::UnexpectedEnd)?;
    let extra = size_extra(b0);
    let end = i + 1 + extra;
    if end > data.len() {
        return Err(ErrorCode::UnexpectedEnd);
    }
    let mut v = (b0 >> 2) as u64;
    let mut k = 0;
    while k < extra {
        v |= (data[i + 1 + k] as u64) << (6 + 8 * k);
        k += 1;
    }
    *pos = end;
    Ok(v)
}

// --- Object keys -----------------------------------------------------------

/// Bytes a struct key occupies on the wire: its size prefix plus its text.
///
/// An object key carries no header, because the object's own header already
/// said the keys are strings.
pub const fn key_len(key: &str) -> usize {
    size_len(key.len() as u64) + key.len()
}

/// The complete `SIZE | DATA` encoding of a struct key.
///
/// `N` must be [`key_len`] of the same key; the macro derives it from exactly
/// that call. Assembling the two halves at compile time makes writing a member
/// one copy of one constant, the same trick the JSON side plays with its
/// pre-quoted `"key":` prefix.
pub const fn encode_key<const N: usize>(key: &str) -> [u8; N] {
    let bytes = key.as_bytes();
    let mut out = [0u8; N];
    let mut head = [0u8; 8];
    let used = encode_size(bytes.len() as u64, &mut head);
    let mut i = 0;
    while i < used {
        out[i] = head[i];
        i += 1;
    }
    let mut j = 0;
    while j < bytes.len() {
        out[used + j] = bytes[j];
        j += 1;
    }
    out
}
