//! Between the formats, without a type in the middle.
//!
//! Everything else here needs a declared type to move a document between the
//! formats: `from_beve::<T>` and then `to_string` will do it, but only for the
//! fields `T` declares and only once `T` exists. BEVE does not need the type
//! at all. Every value states its own kind and its own extent, which is
//! exactly what a JSON writer has to be told, so the walk that reads one drives
//! the other directly.
//!
//! That makes [`beve_to_json`] the answer to "what is actually in this file",
//! which is the thing a binary format is otherwise worst at. There is no tree
//! and no intermediate value: each input byte is read once, and nothing is
//! allocated but the `String` handed back.
//!
//! ```
//! let bytes = structio::to_beve(&vec![1.5f64, 2.5, 3.5]);
//! assert_eq!(structio::beve_to_json(&bytes).unwrap(), "[1.5,2.5,3.5]");
//! ```
//!
//! # What does not survive
//!
//! JSON is the smaller of the two formats, so some BEVE documents have no JSON
//! form. Where one exists this takes it; where none does, it says so rather
//! than inventing one.
//!
//! - **Integer object keys are quoted.** JSON has only string keys, so `1`
//!   becomes `"1"`. That is the same form a `HashMap<u32, _>` already writes
//!   through [`ToJsonKey`](crate::json::ToJsonKey), so the output matches what
//!   the typed path would have produced.
//! - **Non-finite floats become `null`**, which is what the JSON writer does
//!   with them everywhere else.
//! - **Two of the four extensions are refused**, with [`UnsupportedFeature`].
//!   A complex number becomes `[re,im]` and a matrix becomes
//!   `{"layout":…,"extents":[…],"value":[…]}`, which are not encodings invented
//!   here: they are what [`Complex`](crate::Complex) and
//!   [`Matrix`](crate::Matrix) write in JSON and read back from BEVE, so a
//!   document that goes through here still reads into the same types. The other
//!   two hold nothing to write. A delimiter separates documents rather than
//!   being one, and the type tag is deprecated. Both are still *skipped*
//!   correctly by a reader that meets one.
//! - **128-bit floats are refused**, for the same reason [`from_beve`] refuses
//!   them: there is nothing to widen them through.
//!
//! Everything else keeps its value exactly. Integers are written at full width
//! through 128 bits, and the two 16-bit float formats widen losslessly.
//!
//! # Only one direction
//!
//! There is no `json_to_beve`. BEVE prefixes every object and array with its
//! count, and JSON reveals a container's size only at its end, so a pump in
//! that direction has to scan ahead, buffer, or patch, none of which is the
//! straight walk this is. It is also mostly unnecessary: JSON *with* a schema
//! is `from_str::<T>` then [`to_beve`], which recovers typed arrays from the
//! declaration rather than guessing them from the data.
//!
//! [`from_beve`]: crate::from_beve
//! [`to_beve`]: crate::to_beve
//! [`UnsupportedFeature`]: crate::ErrorCode::UnsupportedFeature

use std::io;

use crate::beve::header::{self, byte_width};
use crate::beve::reader::{
    Reader, Typed, bf16_to_f32, complex_payload, f16_to_f32, key_width, le_u128, payload_len,
    sign_extend,
};
use crate::error::{Error, ErrorCode, PResult, Result, StreamError, StreamResult};
use crate::ext::MatrixLayout;
use crate::ext::matrix::{EXTENTS_MEMBER, LAYOUT_MEMBER, VALUE_MEMBER};
use crate::json::Writer;
use crate::options::{Options, Standard};

/// Rewrite a BEVE document as JSON.
///
/// ```
/// # #[derive(Default)]
/// # struct Reading { sensor: String, samples: Vec<f64> }
/// # structio::object!(Reading { sensor, samples });
/// let bytes = structio::to_beve(&Reading {
///     sensor: "thermocouple".into(),
///     samples: vec![21.5, 21.6],
/// });
///
/// assert_eq!(
///     structio::beve_to_json(&bytes).unwrap(),
///     r#"{"sensor":"thermocouple","samples":[21.5,21.6]}"#
/// );
/// ```
///
/// The document must be well formed and must end where its one value does,
/// exactly as [`from_beve`](crate::from_beve) requires. See the [module
/// docs](self) for the handful of BEVE values JSON cannot hold.
pub fn beve_to_json(input: &[u8]) -> Result<String> {
    beve_to_json_with::<Standard>(input)
}

/// [`beve_to_json`] under an explicit [write policy](crate::Options).
///
/// Indenting the result is what makes this the answer to "what is actually in
/// this file": the document has no schema to consult, so the shape has to come
/// off the page.
///
/// ```
/// use structio::{Pretty, to_beve, transcode::beve_to_json_with};
///
/// let doc = to_beve(&vec![1u8, 2]);
/// assert_eq!(beve_to_json_with::<Pretty>(&doc).unwrap(), "[\n  1,\n  2\n]");
/// ```
pub fn beve_to_json_with<O: Options>(input: &[u8]) -> Result<String> {
    let mut w = Writer::<O>::new();
    transcode(input, &mut w)?;
    Ok(w.into_string())
}

/// [`beve_to_json`] into an existing `String`, replacing its contents and
/// keeping its allocation.
///
/// Prefer this when dumping document after document. On failure `out` holds
/// however much was written before the error, the same way
/// [`read_beve_into`](crate::read_beve_into) leaves a partially filled value.
pub fn beve_to_json_into(input: &[u8], out: &mut String) -> Result<()> {
    beve_to_json_into_with::<Standard>(input, out)
}

/// [`beve_to_json_into`] under an explicit [write policy](crate::Options).
pub fn beve_to_json_into_with<O: Options>(input: &[u8], out: &mut String) -> Result<()> {
    let mut buf = core::mem::take(out).into_bytes();
    buf.clear();
    let mut w = Writer::<O>::from_vec(buf);
    let result = transcode(input, &mut w);
    *out = w.into_string();
    result
}

/// [`beve_to_json`] straight into an [`io::Write`].
///
/// The JSON is drained to `out` as it is produced rather than assembled first,
/// so the text never exists in memory all at once. The input still does: a
/// BEVE value is located by stepping over its neighbours, so it has to be a
/// slice.
///
/// A document that fails partway has already handed `out` everything written
/// before the failure. There is no way to take it back, so a caller who needs
/// all-or-nothing should [`validate_beve`](crate::validate_beve) first or write
/// to a buffer.
pub fn beve_to_json_writer<W>(input: &[u8], out: W) -> StreamResult<()>
where
    W: io::Write,
{
    beve_to_json_writer_buffered(input, out, crate::json::writer::DEFAULT_SINK_BUFFER)
}

/// [`beve_to_json_writer`] under an explicit [write policy](crate::Options).
pub fn beve_to_json_writer_with<O, W>(input: &[u8], out: W) -> StreamResult<()>
where
    O: Options,
    W: io::Write,
{
    beve_to_json_writer_buffered_with::<O, W>(input, out, crate::json::writer::DEFAULT_SINK_BUFFER)
}

/// [`beve_to_json_writer`] with an explicit buffer size.
pub fn beve_to_json_writer_buffered<W>(input: &[u8], out: W, buffer: usize) -> StreamResult<()>
where
    W: io::Write,
{
    beve_to_json_writer_buffered_with::<Standard, W>(input, out, buffer)
}

/// [`beve_to_json_writer_buffered`] under an explicit
/// [write policy](crate::Options).
pub fn beve_to_json_writer_buffered_with<O, W>(
    input: &[u8],
    mut out: W,
    buffer: usize,
) -> StreamResult<()>
where
    O: Options,
    W: io::Write,
{
    let mut w = Writer::<O>::to_sink_with_capacity(&mut out, buffer);
    let result = transcode(input, &mut w);
    // `finish` flushes the tail and reports the first I/O failure, and it has
    // to run even when the walk stopped early: a sink writer dropped without
    // it truncates silently. The content error is the more useful of the two,
    // so it is reported first.
    let flushed = w.finish();
    result.map_err(StreamError::Parse)?;
    flushed.map_err(StreamError::Io)
}

/// Walk one whole document, attaching the byte offset to whatever went wrong.
fn transcode<O: Options>(input: &[u8], w: &mut Writer<'_, O>) -> Result<()> {
    let mut r = Reader::new(input);
    match value(&mut r, w).and_then(|()| r.finish()) {
        Ok(()) => Ok(()),
        Err(code) => Err(Error::new(code, r.position())),
    }
}

/// Transcribe the value at the cursor.
fn value<O: Options>(r: &mut Reader<'_>, w: &mut Writer<'_, O>) -> PResult<()> {
    let h = r.head()?;
    body(r, w, h)
}

/// Transcribe a value whose header is already in hand.
///
/// This is `Reader::skip_body` with the payload written out instead of stepped
/// over. It recurses in the same places, charges depth against the same
/// containers, and derives every extent from the same helpers, so it never
/// disagrees with a reader about where a value ends. Where it differs is in
/// refusing what it cannot write.
fn body<O: Options>(r: &mut Reader<'_>, w: &mut Writer<'_, O>, h: u8) -> PResult<()> {
    match header::ty(h) {
        header::TY_NULL_BOOL => match h {
            header::NULL => w.write_null(),
            header::FALSE => w.write_bool(false),
            header::TRUE => w.write_bool(true),
            _ => return Err(ErrorCode::InvalidHeader),
        },
        header::TY_NUMBER => {
            let cat = header::sub(h);
            let code = header::count(h);
            let width = byte_width(cat, code).ok_or(ErrorCode::InvalidHeader)?;
            let bytes = r.take(width)?;
            number(w, cat, code, bytes)?;
        }
        // `str_body` is what validates the UTF-8. It has to: the JSON writer's
        // buffer is handed out as a `String` without revalidation.
        header::TY_STRING => w.write_str(r.str_body()?),
        header::TY_OBJECT => object(r, w, h)?,
        header::TY_TYPED_ARRAY => typed_array(r, w, h)?,
        header::TY_GENERIC_ARRAY => generic_array(r, w)?,
        header::TY_EXTENSION => extension(r, w, h)?,
        _ => return Err(ErrorCode::InvalidHeader),
    }
    Ok(())
}

/// Transcribe an extension, for the two that carry something to write.
///
/// Depth is charged exactly where [`Reader::skip_value`] charges it: none for a
/// complex value, which holds numbers and nothing else, and one level for a
/// matrix, which holds two values.
fn extension<O: Options>(r: &mut Reader<'_>, w: &mut Writer<'_, O>, h: u8) -> PResult<()> {
    match header::ext_id(h) {
        header::EXT_COMPLEX => complex(r, w),
        header::EXT_MATRIX => matrix(r, w),
        // A delimiter separates documents rather than being one, and the type
        // tag is deprecated. See the module docs.
        _ => Err(ErrorCode::UnsupportedFeature),
    }
}

/// Transcribe a complex value: a pair, or an array of pairs.
fn complex<O: Options>(r: &mut Reader<'_>, w: &mut Writer<'_, O>) -> PResult<()> {
    let (class, width, pairs) = r.complex_head()?;
    let cat = header::sub(class);
    let code = header::count(class);
    // Taken whole and walked in place, as a typed array is and for the same
    // reasons: fewer bounds checks, and a bogus count cannot drag this through
    // millions of iterations before the input runs out.
    let payload = r.take(complex_payload(width, pairs)?)?;
    let Some(_) = pairs else {
        return pair(w, cat, code, width, payload);
    };
    w.open(b'[');
    for z in payload.chunks_exact(2 * width) {
        w.item();
        pair(w, cat, code, width, z)?;
        w.push(b',');
    }
    w.close(b']');
    Ok(())
}

/// Write one `[re,im]` out of its two little-endian components.
fn pair<O: Options>(
    w: &mut Writer<'_, O>,
    cat: u8,
    code: u8,
    width: usize,
    z: &[u8],
) -> PResult<()> {
    w.open(b'[');
    w.item();
    number(w, cat, code, &z[..width])?;
    w.push(b',');
    w.item();
    number(w, cat, code, &z[width..2 * width])?;
    // The trailing comma the closer overwrites, exactly as every other
    // container writes one. Ending without it leaves nothing to overwrite and
    // the bracket lands against the last element.
    w.push(b',');
    w.close(b']');
    Ok(())
}

/// Transcribe a matrix as the object both formats read it back from.
///
/// An undefined layout byte is refused rather than written as one of the two
/// that are defined. [`validate`](crate::validate_beve) accepts it, the byte
/// being one byte wherever it points and so no threat to any extent, the same
/// way a valid document may hold a number no target can take.
fn matrix<O: Options>(r: &mut Reader<'_>, w: &mut Writer<'_, O>) -> PResult<()> {
    let layout = MatrixLayout::from_byte(r.take(1)?[0]).ok_or(ErrorCode::InvalidMatrixLayout)?;
    r.enter()?;
    w.open(b'{');
    w.key(LAYOUT_MEMBER);
    w.write_str(layout.as_str());
    w.push(b',');
    w.key(EXTENTS_MEMBER);
    value(r, w)?;
    w.push(b',');
    w.key(VALUE_MEMBER);
    value(r, w)?;
    w.push(b',');
    w.close(b'}');
    r.leave();
    Ok(())
}

/// The bits a 16-bit float is stored as.
///
/// `bytes` reaches [`number`] from a caller rather than from a `take` two lines
/// up, so every width there is confirmed rather than trusted, this one included.
fn half(bytes: &[u8]) -> u16 {
    u16::from_le_bytes(bytes.try_into().expect("2 bytes"))
}

/// Write one number from its little-endian payload.
///
/// Shared by the standalone number and by every element of a fixed-width typed
/// array, which differ only in where the bytes came from. `bytes` is always
/// exactly `byte_width(cat, code)` long, which is what the widths below rely
/// on and what every conversion here re-checks.
fn number<O: Options>(w: &mut Writer<'_, O>, cat: u8, code: u8, bytes: &[u8]) -> PResult<()> {
    match cat {
        header::CAT_FLOAT => match code {
            // No 8-bit float exists, so the two narrowest codes are the two
            // 16-bit ones. See `header::byte_width`.
            //
            // Codes 0 through 2 all widen exactly into an `f32`, and going
            // back out through `write_f32` rather than `write_f64` is what
            // keeps the digits short: the shortest text that round-trips as an
            // `f32` is not the shortest that round-trips as the `f64` it would
            // otherwise have been widened to.
            0 => w.write_f32(bf16_to_f32(half(bytes))),
            1 => w.write_f32(f16_to_f32(half(bytes))),
            2 => w.write_f32(f32::from_le_bytes(bytes.try_into().expect("4 bytes"))),
            3 => w.write_f64(f64::from_le_bytes(bytes.try_into().expect("8 bytes"))),
            // f128 has no Rust counterpart to widen through.
            _ => return Err(ErrorCode::UnsupportedFeature),
        },
        header::CAT_UNSIGNED => w.write_u128(le_u128(bytes)),
        header::CAT_SIGNED => w.write_i128_raw(sign_extend(le_u128(bytes), bytes.len())),
        // A typed array of `CAT_OTHER` never reaches here, and a number header
        // cannot carry it: `byte_width` has already rejected the combination.
        _ => return Err(ErrorCode::ExpectedNumber),
    }
    Ok(())
}

/// Transcribe an object, whatever its keys are made of.
fn object<O: Options>(r: &mut Reader<'_>, w: &mut Writer<'_, O>, h: u8) -> PResult<()> {
    let cat = header::sub(h);
    let width = key_width(h)?;
    let members = r.count()?;
    r.enter()?;
    w.open(b'{');
    for _ in 0..members {
        w.line();
        if cat == header::CAT_FLOAT {
            w.write_str(r.str_body()?);
        } else {
            // JSON has no key but a string, so an integer key is written as its
            // digits inside quotes. That is not so much a lossy choice as the
            // only one, and it is the form `ToJsonKey` already uses, so a
            // `HashMap<u32, _>` transcodes to the bytes it would have been
            // written as directly. `key_width` took its width from the same
            // `byte_width` call, so the payload is what `number` expects.
            w.push(b'"');
            number(w, cat, header::count(h), r.take(width)?)?;
            w.push(b'"');
        }
        w.colon();
        value(r, w)?;
        w.push(b',');
    }
    w.close(b'}');
    r.leave();
    Ok(())
}

/// Transcribe a generic array, whose elements each carry their own header.
fn generic_array<O: Options>(r: &mut Reader<'_>, w: &mut Writer<'_, O>) -> PResult<()> {
    let n = r.count()?;
    r.enter()?;
    w.open(b'[');
    for _ in 0..n {
        w.item();
        value(r, w)?;
        w.push(b',');
    }
    w.close(b']');
    r.leave();
    Ok(())
}

/// Transcribe a typed array, in any of its three shapes.
///
/// It never recurses, its elements being scalars, but it is a container and
/// costs a level like any other. What it does instead of recursing is take the
/// whole payload at once and walk it in place, which is both fewer bounds
/// checks than an element at a time and the reason a bogus element count cannot
/// drag this through millions of iterations before running out of input.
fn typed_array<O: Options>(r: &mut Reader<'_>, w: &mut Writer<'_, O>, h: u8) -> PResult<()> {
    r.enter()?;
    w.open(b'[');
    match r.typed_head(h)? {
        Typed::Bools(n) => {
            let payload = r.take(n.div_ceil(8))?;
            for i in 0..n {
                w.item();
                w.write_bool((payload[i >> 3] >> (i & 7)) & 1 == 1);
                w.push(b',');
            }
        }
        // The one shape that has to be walked rather than indexed, each
        // element carrying its own length.
        Typed::Strings(n) => {
            for _ in 0..n {
                w.item();
                w.write_str(r.str_body()?);
                w.push(b',');
            }
        }
        Typed::Fixed(elem, n) => {
            let cat = header::sub(elem);
            let code = header::count(elem);
            let width = byte_width(cat, code).ok_or(ErrorCode::InvalidHeader)?;
            let payload = r.take(payload_len(elem, n)?)?;
            // `chunks_exact` panics on a zero width; `byte_width` returns at
            // least one byte for every header it accepts at all.
            for chunk in payload.chunks_exact(width) {
                w.item();
                number(w, cat, code, chunk)?;
                w.push(b',');
            }
        }
    }
    w.close(b']');
    r.leave();
    Ok(())
}
