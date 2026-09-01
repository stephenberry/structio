//! Taking the whitespace back out of JSON text.
//!
//! The other direction from [`prettify`](crate::prettify()), and a much smaller
//! job. Laying a document out means knowing its shape, because which container
//! a value sits in decides whether it gets a line or a space. Taking the layout
//! away means knowing only where the strings are: whitespace inside one is the
//! document's, and whitespace outside one is the formatter's.
//!
//! So that is all [`minify`] looks for. It copies runs of bytes through
//! untouched, stopping at a quote to step over a whole string and at whitespace
//! to drop it. Nothing counts brackets, nothing tracks depth, and no token is
//! read: `{"a":01,,,}` comes out `{"a":01,,,}`, because a minifier that refused
//! it would be a validator, and [`from_str`](crate::from_str) is already that.
//! Even a string is measured rather than checked, so a raw control character
//! inside one is copied through like any other byte.
//!
//! Nothing, then, is refused for being wrong. Three things are refused for
//! being unanswerable. A string that never closes, because there is no telling
//! where it ends. A slash that begins no comment, where comments are
//! whitespace, because dropping what follows assumes a comment and keeping it
//! assumes content. And whitespace holding two bare tokens apart, because that
//! whitespace is not the formatter's: removing it would turn `[1 2]` into
//! `[12]`, a different document, and a well-formed one, from input that was
//! neither. Every other input either comes out meaning what it meant or comes
//! out as broken as it went in.
//!
//! [`prettify_with::<Standard>`](crate::json::prettify_with) minifies too, and
//! agrees with this byte for byte on any document that is actually JSON. It
//! walks the structure to get there, so it costs more and it rejects more.
//! Reach for it when the answer matters as much as the output; reach for
//! [`minify`] when there is text to shrink.

use crate::error::{Error, ErrorCode, Result};
use crate::json::parser::{scalar_byte, skip_ws_at};
use crate::json::writer::Writer;
use crate::options::{Options, Standard};
use crate::swar::{eq_mask, find_byte, first_match, load_u64, lt_mask};

/// Strip the insignificant whitespace out of a JSON document.
///
/// ```
/// let out = structio::minify("{\n  \"a\": [1, 2],\n  \"b\": {}\n}").unwrap();
/// assert_eq!(out, r#"{"a":[1,2],"b":{}}"#);
/// ```
///
/// Whitespace inside a string is the document's own and stays:
///
/// ```
/// assert_eq!(structio::minify(r#"[ "a b" ]"#).unwrap(), r#"["a b"]"#);
/// ```
///
/// Neither the structure nor the tokens are checked, so a document that is not
/// JSON usually comes back shorter rather than refused. The exception is
/// whitespace that is holding two tokens apart, which cannot be removed without
/// rewriting the document into a different one:
///
/// ```
/// use structio::ErrorCode;
///
/// let e = structio::minify("[1 2]").unwrap_err();
/// assert_eq!(e.code, ErrorCode::UnexpectedCharacter);
/// assert_eq!(e.index, 3);
/// ```
#[inline]
pub fn minify(input: &str) -> Result<String> {
    minify_with::<Standard>(input)
}

/// [`minify`] under an explicit [policy](crate::Options).
///
/// There is one minified layout, so the write settings have nothing to say
/// here: [`PRETTY`](Options::PRETTY) and [`INDENT`](Options::INDENT) are not
/// read, and `minify_with::<Pretty>` still minifies. The one setting that
/// applies is [`ALLOW_COMMENTS`](Options::ALLOW_COMMENTS), which decides
/// whether `//` and `/* */` are whitespace. They are dropped like any other
/// whitespace, a comment being no part of what a writer can emit. With the
/// setting on, a slash that begins no comment is refused rather than guessed
/// at; with it off, a slash is an ordinary byte and goes through.
///
/// ```
/// use structio::{AllowComments, json::minify_with};
///
/// let out = minify_with::<AllowComments>("[1, /* two */ 2]").unwrap();
/// assert_eq!(out, "[1,2]");
/// ```
#[inline]
pub fn minify_with<O: Options>(input: &str) -> Result<String> {
    let mut out = String::new();
    minify_into_with::<O>(input, &mut out)?;
    Ok(out)
}

/// [`minify`] into an existing `String`, replacing its contents and keeping its
/// allocation.
///
/// Prefer this in a loop, the way [`write_into`](crate::write_into) is
/// preferred over [`to_string`](crate::to_string).
///
/// On failure `out` holds however much was copied before the error, which is
/// the same bargain [`prettify_into`](crate::prettify_into) makes.
#[inline]
pub fn minify_into(input: &str, out: &mut String) -> Result<()> {
    minify_into_with::<Standard>(input, out)
}

/// [`minify_into`] under an explicit [policy](crate::Options).
pub fn minify_into_with<O: Options>(input: &str, out: &mut String) -> Result<()> {
    let mut buf = core::mem::take(out).into_bytes();
    buf.clear();
    // Minifying only ever removes bytes, so the input's length is an exact
    // bound rather than a guess, and this is the only allocation there is. The
    // headroom covers the block `copy_run` stores past the end of a short run.
    buf.reserve(input.len() + BLOCK);

    let mut w = Writer::<O>::from_vec(buf);
    let result = scan::<O>(input.as_bytes(), &mut w);
    // Hand the buffer back whether or not the scan got to the end, so the
    // caller keeps the allocation either way.
    *out = w.into_string();
    result
}

/// Copy `data` through, dropping the whitespace between its tokens.
///
/// An error names its own byte, there being no cursor here to ask: the walk is
/// over indices into `data` rather than over a [`Parser`](crate::json::Parser).
fn scan<O: Options>(data: &[u8], w: &mut Writer<'_, O>) -> Result<()> {
    let n = data.len();
    let mut i = 0;
    // The last run of whitespace seen. Indentation repeats, so the next run is
    // very likely to be a copy of it, and comparing against it steps over the
    // run eight bytes at a time instead of one.
    let (mut ws_start, mut ws_len) = (0usize, 0usize);
    while i < n {
        // Everything up to the next byte worth a decision is already what it
        // should be, and goes through in one copy.
        let start = i;
        i = next_stop::<O>(data, i);
        if i > start {
            copy_run(w, data, start, i);
        }
        if i == n {
            break;
        }
        if data[i] == b'"' {
            // Whole strings, quotes and escapes and all: the input's own bytes,
            // whose extent is the one thing here that has to be got right.
            let end =
                string_end(data, i).ok_or_else(|| Error::new(ErrorCode::UnexpectedEnd, i + 1))?;
            copy_run(w, data, i, end);
            i = end;
        } else {
            let from = if ws_len != 0 && repeats(data, i, ws_start, ws_len) {
                i + ws_len
            } else {
                i
            };
            let after = skip_ws_at::<O>(data, from);
            ws_start = i;
            ws_len = after - i;
            if after == i {
                if O::ALLOW_COMMENTS && data[i] == b'/' {
                    // A slash that begins no comment, where a comment would
                    // have been whitespace. There is no reading of the bytes
                    // after it: dropping them assumes a comment that never
                    // closed, and keeping them assumes content that cannot be
                    // there. Refusing is the only answer that is not a guess.
                    return Err(Error::new(ErrorCode::UnexpectedCharacter, i));
                }
                // A stray control byte, then. Not this function's to judge, so
                // it goes through like every other byte.
                w.push(data[i]);
                i += 1;
            } else {
                // The whole of the strictness. `data[i - 1]` is the last byte
                // emitted, because everything before `i` has gone through
                // verbatim except whitespace, and a run of it ends here.
                if i > 0 && after < n && scalar_byte(data[i - 1]) && scalar_byte(data[after]) {
                    return Err(Error::new(ErrorCode::UnexpectedCharacter, after));
                }
                i = after;
            }
        }
    }
    Ok(())
}

/// Index of the first byte at or after `from` that is not simply copied
/// through, or `data.len()` if there is none.
///
/// Three classes stop the copy: whitespace, which is dropped; a quote, which
/// opens a string that has to be measured rather than scanned past; and, under
/// [`Options::ALLOW_COMMENTS`], a slash. Testing "below `0x21`" covers all four
/// whitespace bytes in one operation and sweeps up stray control characters
/// with them, which the caller then passes through unexamined.
#[inline(always)]
fn next_stop<O: Options>(data: &[u8], from: usize) -> usize {
    let n = data.len();
    let mut i = from;
    while i + 8 <= n {
        // SAFETY: `i + 8 <= n`, so the eight bytes read are in bounds.
        let chunk = unsafe { load_u64(data, i) };
        let mut m = lt_mask(chunk, 0x21) | eq_mask(chunk, b'"');
        if O::ALLOW_COMMENTS {
            m |= eq_mask(chunk, b'/');
        }
        if m != 0 {
            return i + first_match(m);
        }
        i += 8;
    }
    while i < n {
        let c = data[i];
        if c < 0x21 || c == b'"' || (O::ALLOW_COMMENTS && c == b'/') {
            return i;
        }
        i += 1;
    }
    n
}

/// Largest block [`copy_run`] copies at a time, and so the headroom
/// [`minify_into_with`] leaves on the output buffer.
///
/// The headroom is not what makes the copy safe: `append_fixed` asks for its
/// own room and grows if it has to. It is what keeps that from ever happening,
/// so the one reservation stays the only allocation.
const BLOCK: usize = 64;

/// Copy `data[start..end]` to the output.
///
/// The run goes out as one block of a compile-time-constant size, of which only
/// `end - start` bytes are kept. That lowers to a few wide stores rather than a
/// call to `memcpy` with a length the compiler cannot see, which is worth a
/// great deal here: a minifier copies a document in small pieces, a key or a
/// number or a `true` at a time, and the call would cost more than the bytes.
///
/// Two sizes cover what JSON is made of: sixteen bytes takes a key, a small
/// number or a `true`, and sixty-four takes a double or a string of ordinary
/// length. Anything longer is a string long enough to be worth a real `memcpy`,
/// and so is a run close enough to the end of the input that no block fits
/// behind it. A rung between the two was tried and bought nothing.
#[inline(always)]
fn copy_run<O: Options>(w: &mut Writer<'_, O>, data: &[u8], start: usize, end: usize) {
    let len = end - start;
    if block::<O, 16>(w, data, start, len) || block::<O, BLOCK>(w, data, start, len) {
        return;
    }
    w.raw_bytes(&data[start..end]);
}

/// Copy `N` bytes from `data[start..]` and keep `len` of them, if that is a
/// copy this run wants and the input can supply.
#[inline(always)]
fn block<O: Options, const N: usize>(
    w: &mut Writer<'_, O>,
    data: &[u8],
    start: usize,
    len: usize,
) -> bool {
    if len > N {
        return false;
    }
    match data[start..].first_chunk::<N>() {
        Some(block) => {
            w.append_fixed(block, len);
            true
        }
        // Too close to the end of the input for a whole block to come out of it.
        None => false,
    }
}

/// Are the `len` bytes at `at` the same as the `len` bytes at `prev`?
///
/// Asked of a run of whitespace against the run before it, which in a laid-out
/// document is the same indentation as often as not. A match means the bytes at
/// `at` are whitespace too, since the run they equal was, so the whole run can
/// be stepped over without looking at it again.
#[inline(always)]
fn repeats(data: &[u8], at: usize, prev: usize, len: usize) -> bool {
    // A run of fewer than eight bytes is not worth comparing: the byte loop
    // that would follow is the same walk this would be.
    if len < 8 || at + len > data.len() {
        return false;
    }
    let mut k = 0;
    while k + 8 <= len {
        // SAFETY: `k + 8 <= len` and both runs end inside `data`: checked above
        // for `at`, and true of `prev` because the caller records the two
        // together as `ws_start + ws_len == after`, and `after <= data.len()`.
        if unsafe { load_u64(data, at + k) != load_u64(data, prev + k) } {
            return false;
        }
        k += 8;
    }
    // The last eight bytes again, overlapping what has already matched, so a
    // tail of up to seven costs one more compare rather than seven.
    let k = len - 8;
    // SAFETY: `at + len <= data.len()`, and the same holds of `prev`.
    unsafe { load_u64(data, at + k) == load_u64(data, prev + k) }
}

/// Index just past the closing quote of the string whose opening quote is at
/// `open`, or `None` if it never closes.
///
/// The reader's string scan looks for a backslash and a control character
/// alongside the quote, because it has to unescape one and refuse the other.
/// Copying a string through needs neither: an escape is the input's business
/// and goes out as it came in, and the only question is where the string stops.
/// Asking one question instead of three is most of what makes this fast, and
/// strings are most of what a JSON document is.
///
/// A quote closes the string unless an odd number of backslashes escapes it.
/// Counting them backwards is safe because the opening quote is not a
/// backslash, so the walk always stops inside the string.
fn string_end(data: &[u8], open: usize) -> Option<usize> {
    let mut i = open + 1;
    loop {
        let quote = find_byte(data, i, b'"')?;
        let mut back = quote;
        while data[back - 1] == b'\\' {
            back -= 1;
        }
        if (quote - back) % 2 == 0 {
            return Some(quote + 1);
        }
        i = quote + 1;
    }
}
