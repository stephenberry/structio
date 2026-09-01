//! SIMD-within-a-register byte scanning, shared by the parser and the writer.
//!
//! Both sides look for the same three classes of byte in a JSON string body:
//! the closing quote, a backslash, and any control character below `0x20`.
//! Reading eight bytes at a time and testing them with ordinary integer
//! arithmetic finds the first one in a handful of instructions, with no
//! architecture-specific intrinsics and no `unsafe` beyond the load itself.

const ONES: u64 = 0x0101_0101_0101_0101;
const HIGH: u64 = 0x8080_8080_8080_8080;

/// Broadcast one byte across a word.
#[inline(always)]
const fn splat(b: u8) -> u64 {
    ONES.wrapping_mul(b as u64)
}

/// Light the high bit of every byte in `chunk` equal to `b`.
///
/// `(x - ONES) & !x & HIGH` lights the high bit of each zero byte of `x`, so
/// xoring against a broadcast byte first turns it into an equality test.
///
/// Only the **lowest** set bit is guaranteed to mark a real match. The
/// subtraction can borrow out of a matching byte into the one above it, so a
/// higher bit may be spurious, but a borrow only ever travels upward from a
/// genuine match. That makes `trailing_zeros() >> 3` the exact index of the
/// first match, which is all any caller here needs, and it is why these masks
/// are only ever asked where the *first* match is, never which bytes matched.
#[inline(always)]
pub(crate) const fn eq_mask(chunk: u64, b: u8) -> u64 {
    let x = chunk ^ splat(b);
    x.wrapping_sub(ONES) & !x & HIGH
}

/// Light the high bit of every byte in `chunk` below `b`, which must be at
/// most `0x80`.
///
/// The same subtraction as [`eq_mask`] without the xor: a byte below `b`
/// borrows and lands with its high bit set where the original had none. The
/// lowest-set-bit rule applies here too.
#[inline(always)]
pub(crate) const fn lt_mask(chunk: u64, b: u8) -> u64 {
    debug_assert!(b <= 0x80);
    chunk.wrapping_sub(splat(b)) & !chunk & HIGH
}

/// Light the high bit of every byte in `chunk` that a JSON string cannot carry
/// literally: `"`, `\`, or a control character.
///
/// An or of three masks, so the lowest-set-bit rule on [`eq_mask`] carries over
/// to it; see [`first_match`] for why.
#[inline(always)]
pub(crate) const fn escape_mask(chunk: u64) -> u64 {
    eq_mask(chunk, b'"') | eq_mask(chunk, b'\\') | lt_mask(chunk, 0x20)
}

/// Read the eight bytes at `data[i..i + 8]` as a little-endian word.
///
/// # Safety
///
/// `i + 8` must be within `data`.
#[inline(always)]
pub(crate) unsafe fn load_u64(data: &[u8], i: usize) -> u64 {
    debug_assert!(i + 8 <= data.len());
    // SAFETY: the caller guarantees the eight bytes are in bounds. The read is
    // unaligned, which `read_unaligned` permits.
    u64::from_le(unsafe { (data.as_ptr().add(i) as *const u64).read_unaligned() })
}

/// Byte offset of the first match in a mask built by [`eq_mask`] or
/// [`lt_mask`].
///
/// Or by several of them or-ed together, which is still sound: the lowest set
/// bit of the whole is the lowest set bit of whichever mask contributed it, and
/// that one is a genuine match by the rule on [`eq_mask`]. A spurious bit
/// always sits above a genuine one in its own mask, so it can never be the
/// lowest bit of the union either.
#[inline(always)]
pub(crate) const fn first_match(mask: u64) -> usize {
    (mask.trailing_zeros() >> 3) as usize
}

/// Is this a byte a JSON string cannot carry literally?
#[inline(always)]
pub(crate) const fn needs_escape(c: u8) -> bool {
    c < 0x20 || c == b'"' || c == b'\\'
}

/// Offset of the first `b` at or after `from`, if there is one.
///
/// The one place a scan for a single byte lives. Splitting newline-delimited
/// JSON asks it for a newline, since a document boundary there is a newline and
/// nothing else, and the [minifier](crate::minify()) asks it for a quote, since
/// a string ends at one and nothing else says where.
#[inline]
pub(crate) fn find_byte(data: &[u8], from: usize, b: u8) -> Option<usize> {
    let n = data.len();
    let mut i = from;
    while i + 8 <= n {
        // SAFETY: `i + 8 <= n`, so the eight bytes read are in bounds.
        let m = eq_mask(unsafe { load_u64(data, i) }, b);
        if m != 0 {
            return Some(i + first_match(m));
        }
        i += 8;
    }
    while i < n {
        if data[i] == b {
            return Some(i);
        }
        i += 1;
    }
    None
}
