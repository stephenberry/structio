//! Laying out JSON text that is already JSON.
//!
//! Everywhere else in this crate the layout of a document is decided while the
//! document is being produced, by the [write policy](crate::Options) the
//! writer was given. That only helps when the bytes came out of a `Write`
//! impl. A log line, a response body, a file on disk: those arrive as text, and
//! laying them out means reading the text back.
//!
//! [`prettify`] does that in one pass, and does it through the same writer the
//! value path uses. The walk below opens and closes containers with `open` and
//! `close`, breaks a member's line with `line`, spaces its colon with `colon`,
//! and separates elements with `item`, which is the whole whitespace
//! vocabulary [`to_string_with`](crate::to_string_with) has. Prettified text is
//! therefore byte-identical to what writing the same data under the same policy
//! would have produced, and stays that way when a setting is added, because
//! there is no second copy of the rules to keep in step.
//!
//! Values themselves are copied, not re-encoded. A number keeps the spelling
//! the input gave it and a string keeps its escapes, so `1.50` stays `1.50` and
//! `A` stays `A`. The output is the input's data laid out again, not
//! a round trip through this crate's number and string formatters.
//!
//! Structure is checked as the walk goes, because it has to be known to be laid
//! out at all: which container a value is in decides whether it gets a line or
//! a space, and how deep it is decides the indent. A document whose shape does
//! not hold up is an [`Error`] naming the byte that stopped it, rather than
//! output that is quietly wrong.
//!
//! Tokens are not checked past what stepping over them requires. A number is
//! taken by its alphabet, so `01` and `1.2.3` lay out unchanged rather than
//! being refused. Holding them to the grammar instead cost every well-formed
//! number in every document, to move a rejection one step earlier than the
//! reader that will make it anyway. This is not a validator, and
//! [`from_str`](crate::from_str) is the thing to reach for when the question is
//! whether a document is good.

use crate::error::{Error, ErrorCode, PResult, Result};
use crate::json::parser::Parser;
use crate::json::writer::Writer;
use crate::options::{Options, Pretty};

/// Lay out a JSON document across indented lines.
///
/// Two spaces per level, one member or element per line: [`Pretty`], the same
/// policy [`to_string_with`](crate::to_string_with) takes.
///
/// ```
/// let out = structio::prettify(r#"{"a":[1,2],"b":{}}"#).unwrap();
/// assert_eq!(out, "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {}\n}");
/// ```
///
/// The input must be a complete JSON document whose structure holds up.
/// Anything else is an error against the byte that stopped the walk:
///
/// ```
/// use structio::ErrorCode;
///
/// let e = structio::prettify(r#"{"a":}"#).unwrap_err();
/// assert_eq!(e.code, ErrorCode::UnexpectedCharacter);
/// assert_eq!(e.index, 5);
/// ```
#[inline]
pub fn prettify(input: &str) -> Result<String> {
    prettify_with::<Pretty>(input)
}

/// [`prettify`] under an explicit [write policy](crate::Options).
///
/// The policy decides the layout exactly as it does when a value is written:
/// [`INDENT`](Options::INDENT) sets the width and
/// [`NEW_LINES_IN_ARRAYS`](Options::NEW_LINES_IN_ARRAYS) decides whether an
/// array gets a line per element.
///
/// ```
/// use structio::{PrettyInlineArrays, json::prettify_with};
///
/// let out = prettify_with::<PrettyInlineArrays>(r#"{"v":[1,2,3]}"#).unwrap();
/// assert_eq!(out, "{\n  \"v\": [1, 2, 3]\n}");
/// ```
///
/// [`PRETTY`](Options::PRETTY) is honoured too, rather than assumed, so a
/// compact policy compacts. That makes [`Standard`](crate::Standard) a
/// minifier, which is the same walk with nothing to emit between tokens:
///
/// ```
/// use structio::{Standard, json::prettify_with};
///
/// let out = prettify_with::<Standard>("{\n  \"a\": [1, 2]\n}").unwrap();
/// assert_eq!(out, r#"{"a":[1,2]}"#);
/// ```
///
/// [`minify`](crate::minify()) reaches the same bytes on any document that is
/// really JSON, and much faster, because compacting needs none of the structure
/// this walks. Reach for that unless you want the walk's checking too.
///
/// The reading settings do not apply, there being no schema here to have an
/// opinion about: an unknown key is every key. The exception is
/// [`ALLOW_COMMENTS`](Options::ALLOW_COMMENTS), which decides whether a
/// document may carry comments at all. It drops them, as everything in this
/// crate does, a comment being no part of what a writer can emit.
#[inline]
pub fn prettify_with<O: Options>(input: &str) -> Result<String> {
    let mut out = String::new();
    prettify_into_with::<O>(input, &mut out)?;
    Ok(out)
}

/// [`prettify`] into an existing `String`, replacing its contents and keeping
/// its allocation.
///
/// Prefer this in a loop, the way [`write_into`](crate::write_into) is
/// preferred over [`to_string`](crate::to_string).
///
/// On failure `out` holds however much was laid out before the error, which is
/// the same bargain [`read_into`](crate::read_into) makes: recovering the
/// original would mean copying it first, on every call, to serve the failing
/// case.
#[inline]
pub fn prettify_into(input: &str, out: &mut String) -> Result<()> {
    prettify_into_with::<Pretty>(input, out)
}

/// [`prettify_into`] under an explicit [write policy](crate::Options).
pub fn prettify_into_with<O: Options>(input: &str, out: &mut String) -> Result<()> {
    let mut buf = core::mem::take(out).into_bytes();
    // Emptied before the reserve for `minify_into_with`'s reason: `reserve`
    // counts from the length, so clearing has to happen first rather than
    // being left to `Writer::from_vec`.
    buf.clear();
    // Indenting roughly doubles a document of small values, which is the shape
    // that gets prettified; compacting never grows one. Either way this is the
    // only allocation the common case makes.
    buf.reserve(if O::PRETTY {
        input.len().saturating_mul(2)
    } else {
        input.len()
    });

    let mut w = Writer::<O>::from_vec(buf);
    let mut p = Parser::<O>::with_options(input);
    let result = walk(&mut p, &mut w);
    // Hand the buffer back whether or not the walk got to the end, so the
    // caller keeps the allocation either way.
    *out = w.into_string();
    result.map_err(|code| Error::new(code, p.position()))
}

/// One whole document: the value, then nothing but whitespace.
fn walk<O: Options>(p: &mut Parser<'_, O>, w: &mut Writer<'_, O>) -> PResult<()> {
    p.skip_ws();
    value(p, w)?;
    p.finish()
}

/// Lay out one value, whatever it is.
///
/// The cursor must be on the value's first byte, whitespace already behind it.
/// That is the parser's own convention and every route here keeps it: the walk
/// above skips the leading run once, an opening bracket skips what follows it,
/// and `colon` and `comma_or_close` skip what follows them. Skipping again per
/// value would be a third of the whitespace work in a document that has none.
fn value<O: Options>(p: &mut Parser<'_, O>, w: &mut Writer<'_, O>) -> PResult<()> {
    debug_assert!(
        !matches!(p.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')),
        "the cursor is not on a token: whitespace was left unskipped"
    );
    match p.peek() {
        Some(b'{') => object(p, w),
        Some(b'[') => array(p, w),
        _ => copy_token(p, w),
    }
}

/// `{ "key": value, ... }`, one member per line.
fn object<O: Options>(p: &mut Parser<'_, O>, w: &mut Writer<'_, O>) -> PResult<()> {
    p.expect(b'{', ErrorCode::ExpectedBrace)?;
    p.enter()?;
    w.open(b'{');
    p.skip_ws();
    if !p.try_byte(b'}') {
        loop {
            // The cursor is on a token: the `{` above skipped its whitespace,
            // and so does `comma_or_close` at the foot of the loop.
            match p.peek() {
                Some(b'"') => {}
                None => return Err(ErrorCode::UnexpectedEnd),
                Some(_) => return Err(ErrorCode::ExpectedQuote),
            }
            w.line();
            copy_token(p, w)?;
            p.colon()?;
            w.colon();
            value(p, w)?;
            // The unconditional trailing comma every container in this crate
            // writes; `close` below either overwrites it or takes it back.
            w.push(b',');
            if !p.comma_or_close(b'}')? {
                break;
            }
        }
    }
    w.close(b'}');
    p.leave();
    Ok(())
}

/// `[value, ...]`, one element per line or all on one, as the policy says.
fn array<O: Options>(p: &mut Parser<'_, O>, w: &mut Writer<'_, O>) -> PResult<()> {
    p.expect(b'[', ErrorCode::ExpectedBracket)?;
    p.enter()?;
    w.open(b'[');
    p.skip_ws();
    if !p.try_byte(b']') {
        loop {
            w.item();
            value(p, w)?;
            w.push(b',');
            if !p.comma_or_close(b']')? {
                break;
            }
        }
    }
    w.close(b']');
    p.leave();
    Ok(())
}

/// Copy one scalar token through, exactly as the input spelled it.
///
/// A string or a number is the input's own bytes, and its length is not known
/// until the parser has walked it, so it is copied. The three literals are
/// known text and go through the writer's fixed-size appenders instead, which
/// store a compile-time-constant run rather than calling out to a copy of four
/// bytes whose length the compiler cannot see.
///
/// Either way the parser is what finds the token's end, and it checks only what
/// finding the end requires: a string's escapes and its closing quote, a
/// literal's spelling, and a number's alphabet. Object keys come through here
/// too, a key being a string like any other.
///
/// The cursor must already be on the token, as it must be for [`value`].
#[inline]
fn copy_token<O: Options>(p: &mut Parser<'_, O>, w: &mut Writer<'_, O>) -> PResult<()> {
    match p.peek() {
        Some(b't') => {
            p.expect_lit(b"true", ErrorCode::ExpectedTrue)?;
            w.write_bool(true);
        }
        Some(b'f') => {
            p.expect_lit(b"false", ErrorCode::ExpectedFalse)?;
            w.write_bool(false);
        }
        Some(b'n') => {
            p.expect_lit(b"null", ErrorCode::ExpectedNull)?;
            w.write_null();
        }
        _ => {
            // `rest` is tied to the input's lifetime rather than to the borrow,
            // so the token stays reachable across the walk that measures it.
            let from = p.rest();
            let start = p.position();
            p.skip_scalar()?;
            // Whole tokens out of a `&str`, so the buffer stays valid UTF-8.
            w.raw_bytes(&from[..p.position() - start]);
        }
    }
    Ok(())
}
