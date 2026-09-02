//! JSON: text in, text out.
//!
//! The entry points here are re-exported at the crate root, since JSON is the
//! format most callers reach for first. `structio::from_str` and
//! `structio::json::from_str` are the same function.
//!
//! For the binary format that reads and writes the *same* structs, see
//! [`beve`](crate::beve).

pub mod impls;
pub mod minify;
pub mod parser;
pub mod prettify;
pub mod stream;
mod traits;
pub mod writer;

pub use impls::{FromJsonKey, ToJsonKey};
pub use minify::{minify, minify_into, minify_into_with, minify_with};
pub use parser::{JsonStr, Parser};
pub use prettify::{prettify, prettify_into, prettify_into_with, prettify_with};
pub use stream::{
    Documents, Feed, Iter, Mode, from_reader, from_reader_with, to_writer, to_writer_buffered,
    to_writer_buffered_with, to_writer_with,
};
pub use traits::{
    Read, ReadArray, ReadAs, ReadEnum, ReadKeyAs, ReadObject, ReadWrite, Write, WriteArray,
    WriteAs, WriteKeyAs, WriteObject,
};
pub use writer::{Writer, quoted_key};

use crate::error::{Error, ErrorCode, Result};
use crate::options::{Options, Standard};

/// Parse a JSON document into a new value.
///
/// ```
/// # #[derive(Default, Debug, PartialEq)]
/// # struct P { x: i32 }
/// # structio::object!(P { x });
/// let p: P = structio::from_str("{\"x\":1}").unwrap();
/// assert_eq!(p, P { x: 1 });
/// ```
#[inline]
pub fn from_str<'de, T>(input: &'de str) -> Result<T>
where
    T: Read<'de> + Default,
{
    from_str_with::<Standard, T>(input)
}

/// [`from_str`] under an explicit [read policy](crate::Options).
///
/// ```
/// # #[derive(Default, Debug, PartialEq)]
/// # struct P { x: i32 }
/// # structio::object!(P { x });
/// use structio::{SkipUnknown, json::from_str_with};
///
/// let p = from_str_with::<SkipUnknown, P>(r#"{"x":1,"extra":[2,3]}"#).unwrap();
/// assert_eq!(p, P { x: 1 });
/// ```
#[inline]
pub fn from_str_with<'de, O, T>(input: &'de str) -> Result<T>
where
    O: Options,
    T: Read<'de> + Default,
{
    let mut value = T::default();
    read_into_with::<O, T>(&mut value, input)?;
    Ok(value)
}

/// Parse a JSON document into an existing value.
///
/// Prefer this in a loop: the destination keeps its allocations between calls,
/// so repeated parses of the same shape settle into doing no allocation at all.
///
/// On failure `value` is left partially written. That is deliberate, and
/// matches Glaze: recovering the untouched original would mean copying it
/// first, on every call, to serve the failing case.
#[inline]
pub fn read_into<'de, T>(value: &mut T, input: &'de str) -> Result<()>
where
    T: Read<'de>,
{
    read_into_with::<Standard, T>(value, input)
}

/// [`read_into`] under an explicit [read policy](crate::Options).
#[inline]
pub fn read_into_with<'de, O, T>(value: &mut T, input: &'de str) -> Result<()>
where
    O: Options,
    T: Read<'de>,
{
    let mut p = Parser::<O>::with_options(input);
    // Leading whitespace is legal before any value. The container readers skip
    // it themselves, but scalars start reading immediately, so strip it once
    // here rather than making every `Read` impl carry the rule.
    p.skip_ws();
    match value.read(&mut p).and_then(|()| p.finish()) {
        Ok(()) => Ok(()),
        Err(code) => Err(Error::with_key(code, p.position(), p.error_key())),
    }
}

/// Parse a JSON document given as bytes.
///
/// The bytes are checked for UTF-8 once, up front, which is what lets string
/// values be handed back as subslices without any further validation.
#[inline]
pub fn from_slice<'de, T>(input: &'de [u8]) -> Result<T>
where
    T: Read<'de> + Default,
{
    from_slice_with::<Standard, T>(input)
}

/// [`from_slice`] under an explicit [read policy](crate::Options).
#[inline]
pub fn from_slice_with<'de, O, T>(input: &'de [u8]) -> Result<T>
where
    O: Options,
    T: Read<'de> + Default,
{
    let s = core::str::from_utf8(input)
        .map_err(|e| Error::new(ErrorCode::InvalidUtf8, e.valid_up_to()))?;
    from_str_with::<O, T>(s)
}

/// Serialize a value to a `String`.
#[inline]
pub fn to_string<T: Write + ?Sized>(value: &T) -> String {
    to_string_with::<Standard, T>(value)
}

/// [`to_string`] under an explicit [write policy](crate::Options).
///
/// ```
/// # #[derive(Default)]
/// # struct P { x: i32 }
/// # structio::object!(P { x });
/// use structio::{Pretty, json::to_string_with};
///
/// assert_eq!(to_string_with::<Pretty, _>(&P { x: 1 }), "{\n  \"x\": 1\n}");
/// ```
#[inline]
pub fn to_string_with<O: Options, T: Write + ?Sized>(value: &T) -> String {
    let mut w = Writer::<O>::new();
    value.write(&mut w);
    w.into_string()
}

/// Serialize into an existing `String`, replacing its contents and keeping its
/// allocation.
///
/// The contents are this call's to replace, and it takes them before it writes
/// anything, so a `Write` impl that panics leaves `out` empty rather than
/// holding either document. [`append`] is the one that has bytes worth keeping
/// and keeps them.
#[inline]
pub fn write_into<T: Write + ?Sized>(value: &T, out: &mut String) {
    write_into_with::<Standard, T>(value, out);
}

/// [`write_into`] under an explicit [write policy](crate::Options).
#[inline]
pub fn write_into_with<O: Options, T: Write + ?Sized>(value: &T, out: &mut String) {
    let buf = core::mem::take(out).into_bytes();
    let mut w = Writer::<O>::from_vec(buf);
    value.write(&mut w);
    *out = w.into_string();
}

/// Serialize a value after what a buffer already holds.
///
/// [`write_into`] replaces the buffer's contents, so a document that has to
/// sit behind something -- a protocol header, or the entries already written
/// into a listing -- has until now needed a second buffer and a copy out of
/// it. This writes past what is there instead, into the one allocation, and is
/// the JSON counterpart of [`beve::append`](crate::beve::append).
///
/// The bytes in front are neither examined nor required to be text, which is
/// why this takes the buffer as bytes. [`Writer::appending`] is the same thing
/// with the writer in hand, and is the way to append into a `String`.
///
/// They are not lost either, if writing the value panics: `out` comes back
/// holding exactly what it held before the call. A `Write` impl may panic by
/// design, an adapter whose target has values it cannot encode being told to
/// substitute or panic, and the bytes in front are the caller's rather than
/// this call's to spend. [`write_into`] makes the other bargain, its buffer
/// being one it was asked to overwrite.
///
/// ```
/// # #[derive(Default)]
/// # struct Reading { id: u32 }
/// # structio::object!(Reading { id });
/// let mut frame = vec![0u8; 8]; // a header, already written
/// structio::append(&Reading { id: 7 }, &mut frame);
///
/// assert_eq!(&frame[8..], br#"{"id":7}"#);
/// ```
#[inline]
pub fn append<T: Write + ?Sized>(value: &T, out: &mut Vec<u8>) {
    append_with::<Standard, T>(value, out);
}

/// [`append`] under an explicit [write policy](crate::Options).
#[inline]
pub fn append_with<O: Options, T: Write + ?Sized>(value: &T, out: &mut Vec<u8>) {
    writer::append_in_place(out, Writer::<O>::appending, |w| value.write(w));
}

/// Serialize to a byte vector.
#[inline]
pub fn to_vec<T: Write + ?Sized>(value: &T) -> Vec<u8> {
    to_vec_with::<Standard, T>(value)
}

/// [`to_vec`] under an explicit [write policy](crate::Options).
#[inline]
pub fn to_vec_with<O: Options, T: Write + ?Sized>(value: &T) -> Vec<u8> {
    let mut w = Writer::<O>::new();
    value.write(&mut w);
    w.into_vec()
}
