//! BEVE: the same structs, as tagged binary.
//!
//! [BEVE](https://github.com/stephenberry/beve) keeps everything that makes
//! JSON workable between programs that do not share a schema. Values are
//! tagged, objects are keyed, and a document describes itself well enough to
//! be skipped through without knowing what it holds. What it drops is the
//! text: a number is stored as a number at its own width, a string is stored
//! with a length instead of a terminator and escapes, and a run of numbers is
//! stored as one header and one block of bytes.
//!
//! ```
//! # #[derive(Default, Debug, PartialEq)]
//! # struct Reading { sensor: String, samples: Vec<f64> }
//! # structio::object!(Reading { sensor, samples });
//! let reading = Reading {
//!     sensor: "thermocouple".into(),
//!     samples: vec![21.5, 21.6, 21.4],
//! };
//!
//! let bytes = structio::to_beve(&reading);
//! assert_eq!(structio::from_beve::<Reading>(&bytes).unwrap(), reading);
//! ```
//!
//! The entry points are named for what they do, not for the format, and the
//! crate root re-exports them with the format in the name. [`beve::to_vec`]
//! and [`structio::to_beve`](crate::to_beve) are the same function.
//!
//! [`beve::to_vec`]: to_vec
//!
//! # Where the wins are
//!
//! A `Vec<f64>` is one header, one count, and the slice's own bytes, so
//! writing it is a `memcpy` and reading it back into a `Vec<f64>` is another.
//! The same holds for every fixed-width numeric type, for `Vec<bool>` (packed
//! one per bit), and for `Vec<String>` (no per-element header). A `&'de str`
//! or `&'de [u8]` field borrows straight out of the input buffer, with no copy
//! at all, because BEVE stores both verbatim.
//!
//! [`to_vec_aligned`] goes one further and pads each numeric payload onto its
//! own element width, which is what BEVE's aligned form is for: a block laid
//! out that way is one a reader can point at rather than copy. The same
//! document either way, and every reader here takes both forms. A
//! `Cow<'de, [f64]>` field is the one that takes the borrow when the document
//! allows it, through [`Reader::try_slice`].
//!
//! # Looking at a document without decoding it
//!
//! Every value states its own extent, which makes two things cheap that JSON
//! cannot offer. [`from_slice_at`] reads the one value a JSON Pointer names,
//! stepping over everything off the path and indexing a typed array by
//! multiplying rather than by walking it. [`validate`] confirms a document is
//! well formed in one pass, without turning any of it into a Rust type and
//! without allocating.
//!
//! Both still answer in terms you supply: a type for [`from_slice_at`] to fill,
//! or a yes or no from [`validate`]. [`beve_to_json`](crate::beve_to_json) is
//! the one that hands back the contents themselves, rewriting a document as
//! JSON in one walk, which is how you read one you have no type for.
//!
//! # Documents that do not fit
//!
//! [`from_slice`] wants the whole document, and [`from_reader`] only hides the
//! `read_to_end` that puts it there. [`stream`] is the answer to a file too
//! large to hold: [`Documents`] hands out one value at a time, [`Feed`] does
//! the same for bytes pushed at you, and both stream a typed array element by
//! element rather than buffering the block whole.
//!
//! One whole value is their floor, so a document that *is* one enormous
//! numeric array has [`read_array_into`] instead. It moves the payload from
//! the reader into the destination vector directly, which leaves the vector as
//! the only thing resident and keeps the single copy that reading such an
//! array is worth doing for.
//!
//! Writing has [`to_writer`], which drains as it produces, and [`size`] beside
//! it for the frame whose header states the body's length *before* the body:
//! it reports exactly what that write will emit without producing any of it,
//! so nothing has to be staged in memory to be measured.
//!
//! # Complex numbers and matrices
//!
//! The two extensions that carry data have types of their own:
//! [`Complex`](crate::Complex) for a complex number and
//! [`Matrix`](crate::Matrix) for an array that knows its own shape. A run of
//! complex numbers is one header and one block, so a `Vec<Complex<f64>>` moves
//! in a single copy exactly as a `Vec<f64>` does. See [`ext`](crate::ext).
//!
//! # What it does not do yet
//!
//! The delimiter and the deprecated type tag hold no data and get no types.
//! Both are still stepped over correctly wherever they appear, so a document
//! carrying one in a field you do not want stays readable for the fields you
//! do.

use std::io;

use crate::error::{Error, Result, StreamError, StreamResult};
use crate::options::{Measured, Options, Standard};

pub mod header;
pub mod impls;
pub mod reader;
pub mod stream;
mod traits;
pub mod writer;

pub use impls::{FromBeveKey, NumericBytes, ToBeveKey};
pub use reader::{Key, MAX_DEPTH, Reader};
pub use stream::{Documents, Feed, Iter, Mode, from_reader_array, read_array_into};
pub use traits::{
    Read, ReadArray, ReadAs, ReadEnum, ReadInternallyTagged, ReadKeyAs, ReadObject, ReadWrite,
    Write, WriteArray, WriteAs, WriteKeyAs, WriteObject,
};
pub use writer::Writer;

/// Parse a BEVE document into a new value.
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
    let mut value = T::default();
    read_into_with::<O, T>(&mut value, input)?;
    Ok(value)
}

/// Parse a BEVE document into an existing value.
///
/// Prefer this in a loop: the destination keeps its allocations between calls.
/// On failure `value` is left partially written, exactly as on the JSON side.
#[inline]
pub fn read_into<'de, T>(value: &mut T, input: &'de [u8]) -> Result<()>
where
    T: Read<'de>,
{
    read_into_with::<Standard, T>(value, input)
}

/// [`read_into`] under an explicit [read policy](crate::Options).
#[inline]
pub fn read_into_with<'de, O, T>(value: &mut T, input: &'de [u8]) -> Result<()>
where
    O: Options,
    T: Read<'de>,
{
    let mut r = Reader::<O>::with_options(input);
    match value.read(&mut r).and_then(|()| r.finish()) {
        Ok(()) => Ok(()),
        Err(code) => Err(Error::with_key(code, r.position(), r.error_key())),
    }
}

/// Parse one value out of a BEVE document, identified by a [JSON Pointer].
///
/// Everything not on the path is stepped over rather than parsed, so the cost
/// is a walk over the headers in front of the value and then one read of the
/// value itself. Reaching one field of a large document does not decode the
/// rest of it, and does not allocate for the parts it passes.
///
/// The empty pointer names the whole document, and is [`from_slice`] with an
/// extra argument. Anything else is `/` followed by one token per level:
/// `/servers/0/port` is the `port` of the first element of `servers`. A key
/// containing `/` or `~` is spelled with `~1` and `~0`, and an object with
/// integer keys takes an integer token.
///
/// ```
/// # #[derive(Default, Debug, PartialEq)]
/// # struct Server { host: String, port: u16 }
/// # structio::object!(Server { host, port });
/// # #[derive(Default, Debug, PartialEq)]
/// # struct Config { servers: Vec<Server> }
/// # structio::object!(Config { servers });
/// # let config = Config { servers: vec![
/// #     Server { host: "a".into(), port: 80 },
/// #     Server { host: "b".into(), port: 443 },
/// # ] };
/// let bytes = structio::to_beve(&config);
///
/// let port: u16 = structio::from_beve_at(&bytes, "/servers/1/port").unwrap();
/// assert_eq!(port, 443);
/// ```
///
/// Unlike [`from_slice`] this does not require the document to end where the
/// value does, the rest of it being what surrounds the value asked for. The
/// bytes after it are therefore never looked at, so a document that is
/// malformed past the value it names is still read from successfully. Use
/// [`validate`] first where that matters.
///
/// A well-formed pointer naming something the document does not hold is
/// [`NoSuchValue`]; a pointer that is not well formed is [`InvalidPointer`].
///
/// [JSON Pointer]: https://www.rfc-editor.org/rfc/rfc6901
/// [`NoSuchValue`]: crate::ErrorCode::NoSuchValue
/// [`InvalidPointer`]: crate::ErrorCode::InvalidPointer
#[inline]
pub fn from_slice_at<'de, T>(input: &'de [u8], pointer: &str) -> Result<T>
where
    T: Read<'de> + Default,
{
    from_slice_at_with::<Standard, T>(input, pointer)
}

/// [`from_slice_at`] under an explicit [read policy](crate::Options).
///
/// The policy governs the value the pointer names. Everything on the way to it
/// is stepped over rather than read, so an unknown key beside the path is not
/// an unknown key to anything.
#[inline]
pub fn from_slice_at_with<'de, O, T>(input: &'de [u8], pointer: &str) -> Result<T>
where
    O: Options,
    T: Read<'de> + Default,
{
    let mut value = T::default();
    read_into_at_with::<O, T>(&mut value, input, pointer)?;
    Ok(value)
}

/// [`from_slice_at`] into an existing value, keeping its allocations.
///
/// Prefer this to pull the same field out of document after document.
#[inline]
pub fn read_into_at<'de, T>(value: &mut T, input: &'de [u8], pointer: &str) -> Result<()>
where
    T: Read<'de>,
{
    read_into_at_with::<Standard, T>(value, input, pointer)
}

/// [`read_into_at`] under an explicit [read policy](crate::Options).
#[inline]
pub fn read_into_at_with<'de, O, T>(value: &mut T, input: &'de [u8], pointer: &str) -> Result<()>
where
    O: Options,
    T: Read<'de>,
{
    let mut r = Reader::<O>::with_options(input);
    match r.seek(pointer).and_then(|()| value.read(&mut r)) {
        Ok(()) => Ok(()),
        Err(code) => Err(Error::with_key(code, r.position(), r.error_key())),
    }
}

/// Borrow a document that is one numeric array as `&[T]`, copying nothing.
///
/// This is [`Reader::try_slice`] over a whole document rather than at a
/// cursor: the array has to be all of it, so trailing bytes decline the borrow
/// the same way a mismatched element type does.
///
/// `None` is not an error report. The three conditions a borrow needs are
/// listed on [`Reader::try_slice`], and two of them are properties of the
/// machine rather than of the document, so the same bytes can be borrowable on
/// one run and not the next. Fall back to [`from_slice`], which decodes a copy
/// and says what is wrong if the document is what is wrong.
///
/// Write the document with [`to_vec_aligned`], or with [`append_aligned`]
/// where it goes behind a header, to give the payload a chance of landing
/// where a `&[T]` can point. That settles the offset within the document; the
/// document's own address is the allocator's business, which is why this
/// declines rather than fails.
///
/// For an array nested inside a larger document, borrow at the cursor:
/// [`Reader::new`], [`seek`](Reader::seek), then
/// [`try_slice`](Reader::try_slice).
///
/// ```
/// let doc = structio::to_beve_aligned(&vec![1.0f64, 2.0, 3.0]);
/// match structio::beve_slice_ref::<f64>(&doc) {
///     Some(xs) => assert_eq!(xs, [1.0, 2.0, 3.0]),
///     // Not this time. The copy is always available.
///     None => assert_eq!(structio::from_beve::<Vec<f64>>(&doc).unwrap(), [1.0, 2.0, 3.0]),
/// }
/// ```
///
/// There is no `_with` form. A [read policy](crate::Options) settles what to
/// do about object keys the other side did or did not send, and a typed array
/// has none.
#[inline]
pub fn slice_ref<T: NumericBytes>(input: &[u8]) -> Option<&[T]> {
    let mut r = Reader::new(input);
    let block = r.try_slice::<T>()?;
    r.finish().ok()?;
    Some(block)
}

/// Check that `input` is one well-formed BEVE document, without decoding it.
///
/// Every header, every length, every nested value, and every string's UTF-8 is
/// checked. Nothing is turned into a Rust type and nothing is allocated, so
/// this costs one walk over the bytes and no memory, whatever the document
/// holds.
///
/// Well formed here means *exactly one* value with no trailing bytes, which is
/// what [`from_slice`] requires too. A run of delimiter-separated values is
/// several documents rather than one, and is reported as trailing content.
///
/// Validity is a property of the bytes, not of any type you have declared, so
/// this says nothing about whether some `T` can read them. Use it on input
/// from somewhere you do not trust before handing it on, or to tell a
/// corrupted document apart from one that simply does not match your schema.
///
/// One thing a valid document can carry that this still refuses: an extension
/// beyond the four the specification defines. Its extent is unknown, so the
/// bytes after it cannot be located and the rest of the document cannot be
/// checked at all. That is [`UnsupportedFeature`], which is distinguishable
/// from the codes a genuinely malformed document produces.
///
/// ```
/// let bytes = structio::to_beve(&vec![1u32, 2, 3]);
/// assert!(structio::validate_beve(&bytes).is_ok());
/// assert!(structio::validate_beve(&bytes[..bytes.len() - 1]).is_err());
/// ```
///
/// [`UnsupportedFeature`]: crate::ErrorCode::UnsupportedFeature
pub fn validate(input: &[u8]) -> Result<()> {
    let mut r = Reader::new(input);
    match r.validate_value().and_then(|()| r.finish()) {
        Ok(()) => Ok(()),
        Err(code) => Err(Error::new(code, r.position())),
    }
}

/// [`validate`] over an [`io::Read`].
///
/// The reader is drained into memory first, exactly as [`from_reader`] does,
/// so this is a convenience rather than a way to check a document larger than
/// memory. There is no streaming counterpart: validity is a property of one
/// whole document, and [`Documents`] deals in the values inside one.
pub fn validate_reader<R: io::Read>(mut reader: R) -> StreamResult<()> {
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    validate(&buf).map_err(StreamError::Parse)
}

/// Serialize a value to a byte vector.
#[inline]
pub fn to_vec<T: Write + ?Sized>(value: &T) -> Vec<u8> {
    to_vec_with::<Standard, T>(value)
}

/// [`to_vec`] under an explicit [write policy](crate::Options).
///
/// [`Options::PRETTY`] means nothing to a binary format and is ignored;
/// [`Options::SKIP_NULL`] applies exactly as it does to JSON.
#[inline]
pub fn to_vec_with<O: Options, T: Write + ?Sized>(value: &T) -> Vec<u8> {
    let mut w = Writer::<O>::new();
    value.write(&mut w);
    w.into_vec()
}

/// Serialize a value with its numeric typed arrays in the aligned form.
///
/// The document [`to_vec`] writes, laid out so that a reader can point at its
/// arrays instead of copying them out, which is what [`Reader::try_slice`]
/// then does with it. [`Writer::aligned`] says what changes and what it costs;
/// this is that writer over a fresh buffer, and every other entry point is
/// reached by building one:
///
/// ```
/// use structio::Standard;
/// use structio::beve::{Write, Writer};
///
/// let samples = vec![1.5f64, 2.5, 3.5];
/// let mut out = Vec::new();
/// let mut w = Writer::<Standard>::to_sink(&mut out).aligned();
/// samples.write(&mut w);
/// w.finish()?;
///
/// assert_eq!(out, structio::to_beve_aligned(&samples));
/// assert_eq!(structio::from_beve::<Vec<f64>>(&out).unwrap(), samples);
/// # Ok::<(), std::io::Error>(())
/// ```
#[inline]
pub fn to_vec_aligned<T: Write + ?Sized>(value: &T) -> Vec<u8> {
    to_vec_aligned_with::<Standard, T>(value)
}

/// [`to_vec_aligned`] under an explicit [write policy](crate::Options).
#[inline]
pub fn to_vec_aligned_with<O: Options, T: Write + ?Sized>(value: &T) -> Vec<u8> {
    let mut w = Writer::<O>::new().aligned();
    value.write(&mut w);
    w.into_vec()
}

/// Serialize into an existing buffer, replacing its contents and keeping its
/// allocation.
///
/// The contents are this call's to replace, and it takes them before it writes
/// anything, so a `Write` impl that panics leaves `out` empty rather than
/// holding either document. [`append`] is the one that has bytes worth keeping
/// and keeps them.
#[inline]
pub fn write_into<T: Write + ?Sized>(value: &T, out: &mut Vec<u8>) {
    write_into_with::<Standard, T>(value, out);
}

/// [`write_into`] under an explicit [write policy](crate::Options).
#[inline]
pub fn write_into_with<O: Options, T: Write + ?Sized>(value: &T, out: &mut Vec<u8>) {
    let buf = core::mem::take(out);
    let mut w = Writer::<O>::from_vec(buf);
    value.write(&mut w);
    *out = w.into_vec();
}

/// Serialize a value after what a buffer already holds.
///
/// [`write_into`] replaces the buffer's contents, so a document that has to sit
/// behind a header has until now needed a second buffer and a copy out of it.
/// This writes past what is there instead, into the one allocation.
///
/// The bytes already in the buffer also count as the part of the document in
/// front of this value, so an implementation that positions its own bytes from
/// [`Writer::offset`] lands them where they will really sit. [`size_after`] is
/// the matching measurement: `size_after(value, out.len())` is what this will
/// add.
///
/// They are not lost either, if writing the value panics: `out` comes back
/// holding exactly what it held before the call. A `Write` impl may panic by
/// design, an adapter whose target has values it cannot encode being told to
/// substitute or panic, and the bytes in front are the caller's rather than
/// this call's to spend. [`write_into`] makes the other bargain, its buffer
/// being one it was asked to overwrite.
#[inline]
pub fn append<T: Write + ?Sized>(value: &T, out: &mut Vec<u8>) {
    append_with::<Standard, T>(value, out);
}

/// [`append`] under an explicit [write policy](crate::Options).
#[inline]
pub fn append_with<O: Options, T: Write + ?Sized>(value: &T, out: &mut Vec<u8>) {
    writer::append_in_place(out, Writer::<O>::appending, |w| value.write(w));
}

/// Serialize a value's aligned form after what a buffer already holds.
///
/// [`to_vec_aligned`] into a buffer with something in front of it. The padding
/// that lands each numeric payload on its own element width is measured from
/// the start of the document, and the document starts where this buffer does,
/// so a value appended behind a fixed header and a variable-length prefix is
/// padded against the length of both. That is the whole difference from
/// [`to_vec_aligned`], which has nothing in front of it and counts from zero.
///
/// ```
/// let samples = vec![1.5f64, 2.5, 3.5];
/// let mut frame = vec![0u8; 12]; // a header, already written
///
/// let body = structio::beve_size_aligned_after(&samples, frame.len());
/// structio::append_beve_aligned(&samples, &mut frame);
///
/// assert_eq!(frame.len(), 12 + body);
/// assert_eq!(structio::from_beve::<Vec<f64>>(&frame[12..]).unwrap(), samples);
/// ```
///
/// [`size_aligned_after`] is the matching measurement, as above, and a panic
/// out of the value's `Write` impl leaves `out` as it was, as in [`append`].
///
/// This takes the buffer for the document, which is right for a message
/// assembled in one buffer and wrong for the two cases where it is not. A body
/// built on its own to be concatenated behind a header later has nothing in
/// front of it here; a frame appended to a send buffer that still holds the
/// frames before it has too much. Neither is a mistake this can detect, and
/// both come out padded for a document nobody will read as one, so say where
/// the value stands and use [`Writer`] directly:
///
/// ```
/// use structio::Standard;
/// use structio::beve::{Write, Writer};
///
/// let samples = vec![1.5f64, 2.5];
/// let mut send = vec![0u8; 100];       // frames already queued
/// let frame_start = send.len();
/// send.extend_from_slice(&[0u8; 48]);  // this frame's header
///
/// // Where the body stands in *this frame*, not in the send buffer.
/// let at = send.len() - frame_start;
/// let body = structio::beve_size_aligned_after(&samples, at);
///
/// let mut w = Writer::<Standard>::appending(send).aligned().at(at);
/// samples.write(&mut w);
/// let send = w.into_vec();
///
/// assert_eq!(send.len() - frame_start - 48, body);
/// assert_eq!(structio::from_beve::<Vec<f64>>(&send[frame_start + 48..]).unwrap(), samples);
/// ```
#[inline]
pub fn append_aligned<T: Write + ?Sized>(value: &T, out: &mut Vec<u8>) {
    append_aligned_with::<Standard, T>(value, out);
}

/// [`append_aligned`] under an explicit [write policy](crate::Options).
#[inline]
pub fn append_aligned_with<O: Options, T: Write + ?Sized>(value: &T, out: &mut Vec<u8>) {
    writer::append_in_place(
        out,
        |buf| Writer::<O>::appending(buf).aligned(),
        |w| value.write(w),
    );
}

/// Serialize a value straight into an [`io::Write`].
///
/// The document is drained to `out` as it is produced rather than assembled
/// first, so this holds [`DEFAULT_SINK_BUFFER`](writer::DEFAULT_SINK_BUFFER)
/// bytes and no more, however large the document or any single value in it. A
/// long string or a large typed array is one contiguous block, and a block
/// that would not fit the buffer is handed to `out` directly rather than
/// copied in first.
///
/// Wrapping `out` in a [`BufWriter`](std::io::BufWriter) is redundant: this
/// already buffers and hands the sink whole blocks. `out` is not flushed.
///
/// The error is `out`'s own, handed back unchanged. A [`Write`] implementation
/// returns nothing to fail with, so the sink is the only thing on this path
/// that can go wrong and there is no crate error type to fold it into. Where
/// one direction of I/O does meet a parse failure, reading through a reader
/// and [`beve_to_json_writer`](crate::beve_to_json_writer), the type that
/// carries both is [`StreamError`]; it converts from [`io::Error`] with `?`,
/// so a caller wanting one error type across both directions can use it here
/// too.
pub fn to_writer<T, W>(value: &T, out: W) -> io::Result<()>
where
    T: Write + ?Sized,
    W: io::Write,
{
    to_writer_with::<Standard, T, W>(value, out)
}

/// [`to_writer`] under an explicit [write policy](crate::Options).
pub fn to_writer_with<O, T, W>(value: &T, mut out: W) -> io::Result<()>
where
    O: Options,
    T: Write + ?Sized,
    W: io::Write,
{
    let mut w = Writer::<O>::to_sink(&mut out);
    value.write(&mut w);
    w.finish()
}

/// [`to_writer`] with an explicit buffer size.
pub fn to_writer_buffered<T, W>(value: &T, out: W, buffer: usize) -> io::Result<()>
where
    T: Write + ?Sized,
    W: io::Write,
{
    to_writer_buffered_with::<Standard, T, W>(value, out, buffer)
}

/// [`to_writer_buffered`] under an explicit [write policy](crate::Options).
pub fn to_writer_buffered_with<O, T, W>(value: &T, mut out: W, buffer: usize) -> io::Result<()>
where
    O: Options,
    T: Write + ?Sized,
    W: io::Write,
{
    let mut w = Writer::<O>::to_sink_with_capacity(&mut out, buffer);
    value.write(&mut w);
    w.finish()
}

/// The number of bytes [`to_vec`] would produce, without producing them.
///
/// For a frame whose header states the body's length *before* the body: measure
/// the value, write the header, then write the body straight to the sink. There
/// is no intermediate buffer to fill and none to copy out of.
///
/// ```
/// # #[derive(Default)]
/// # struct Reading { sensor: String, samples: Vec<f64> }
/// # structio::object!(Reading { sensor, samples });
/// # let reading = Reading { sensor: "thermocouple".into(), samples: vec![21.5, 21.6] };
/// let mut out: Vec<u8> = Vec::new();
///
/// let body = structio::beve_size(&reading);
/// out.extend_from_slice(&(body as u32).to_le_bytes());
/// structio::to_beve_writer(&reading, &mut out)?;
///
/// assert_eq!(out.len(), 4 + body);
/// assert_eq!(&out[4..], structio::to_beve(&reading));
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// # Why the two agree
///
/// Because they are the same code. This is [`Writer`] with its stores taken
/// out: the same [`Write`] implementations, driven through the same methods,
/// with every header, count, key and padding byte decided by the same lines
/// that decide them when the bytes are kept. What changes is that an append
/// adds its own length to a counter instead of copying anything. There is no
/// second description of the format to fall out of step with the first.
///
/// A hand-written [`Write`] impl is carried along with everything else, on one
/// condition: that it decides nothing from how much has been *buffered*.
/// [`Writer::len`] and [`Writer::as_bytes`] report the buffer, which a sink
/// writer empties on every drain and a measuring one never fills, so an
/// implementation laying out its own value wants [`Writer::offset`] -- the
/// position in the document, correct under both. Reading the buffer while
/// measuring trips a debug assertion rather than quietly returning zero.
///
/// The policy has to be the one you will write under, which is what
/// [`size_with`] is for: [`Options::SKIP_NULL`] changes how many members an
/// object holds. So does the form, which is what [`size_aligned`] is for, and
/// so does where the value lands, which is what [`size_after`] and
/// [`size_aligned_after`] are for.
///
/// # What it costs
///
/// No allocation, and not one byte of output touched. Sizes that are constants
/// stay constants: a struct of fixed-width numbers folds to a literal, and a
/// typed array is its count times its element width. What is left to walk is
/// the part whose size genuinely depends on the data, which is strings,
/// generic arrays, maps, and members [`Options::SKIP_NULL`] may drop.
///
/// It is still a walk, so it is not the cheaper way to frame a value you
/// *could* buffer. Writing into a reused `Vec` with [`write_into`] and taking
/// its length is one walk where measuring first is two. Reach for this when
/// the length has to reach the wire before the bytes do, or when the
/// destination has a capacity to check against and no room to stage the body.
#[inline]
pub fn size<T: Write + ?Sized>(value: &T) -> usize {
    size_after_with::<Standard, T>(value, 0)
}

/// [`size`] under an explicit [write policy](crate::Options).
///
/// The policy must be the one the value will be written under, or the answer
/// describes a different document: `size_with::<P, _>(v)` is
/// `to_vec_with::<P, _>(v).len()` and says nothing about any other `P`.
#[inline]
pub fn size_with<O: Options, T: Write + ?Sized>(value: &T) -> usize {
    size_after_with::<O, T>(value, 0)
}

/// The number of bytes [`append`] would add to a buffer of length `prefix`.
///
/// [`size`] for a value that will not begin the document. `prefix` is the
/// length of everything in front of it, so `size_after(value, 0)` is [`size`]
/// exactly.
///
/// For the plain form the two agree whatever `prefix` is, since nothing built into
/// this crate looks at where it sits. What the offset is for is an
/// implementation that positions its own bytes from [`Writer::offset`], which
/// measures differently depending on where it lands and would otherwise be
/// measured as though it began the document. [`size_aligned_after`] is the form
/// where the offset always matters.
#[inline]
pub fn size_after<T: Write + ?Sized>(value: &T, prefix: usize) -> usize {
    size_after_with::<Standard, T>(value, prefix)
}

/// [`size_after`] under an explicit [write policy](crate::Options).
#[inline]
pub fn size_after_with<O: Options, T: Write + ?Sized>(value: &T, prefix: usize) -> usize {
    let mut w = Writer::<Measured<O>>::new().at(prefix);
    value.write(&mut w);
    w.measured() - prefix
}

/// The number of bytes [`to_vec_aligned`] would produce.
///
/// [`size`] for the aligned form, whose numeric payloads are padded onto their
/// own element width. The padding depends on where each payload lands, so this
/// counts from offset zero exactly as [`to_vec_aligned`] writes. A value that
/// will land behind a prefix is padded against that prefix instead and measured
/// by [`size_aligned_after`]; the two disagree by as much as fifteen bytes per
/// array, so the one to use is the one that matches the write.
#[inline]
pub fn size_aligned<T: Write + ?Sized>(value: &T) -> usize {
    size_aligned_after_with::<Standard, T>(value, 0)
}

/// [`size_aligned`] under an explicit [write policy](crate::Options).
#[inline]
pub fn size_aligned_with<O: Options, T: Write + ?Sized>(value: &T) -> usize {
    size_aligned_after_with::<O, T>(value, 0)
}

/// The number of bytes [`append_aligned`] would add to a buffer of length
/// `prefix`.
///
/// [`size_aligned`] for a value that will not begin the document, which is what
/// a length-prefixed frame makes of one: the header goes out first, so the body
/// starts behind it, and the padding in front of every numeric payload is
/// chosen from where the payload lands in the frame. Measuring at zero and
/// writing behind a prefix describe different documents, and the difference is
/// a `body_length` the far end cannot use.
///
/// A fixed header alone will not show you this. A 48-byte header changes
/// nothing at all, since every element width divides 48; it is the
/// variable-length part behind it -- a route, a query, an identifier -- that
/// moves the payload off its width. So the offset that matters is the whole of
/// what precedes the body, and a frame that measures correctly for one route
/// length will not for the next.
///
/// ```
/// # let samples = vec![1.5f64, 2.5, 3.5];
/// # let mut frame = vec![0u8; 12];
/// let body = structio::beve_size_aligned_after(&samples, frame.len());
/// structio::append_beve_aligned(&samples, &mut frame);
/// assert_eq!(frame.len(), 12 + body);
/// ```
///
/// Whether the payloads are then on their element width *in memory* is a
/// separate question, and the reader's: it needs the buffer's own address to be
/// aligned, which no allocator promises for a `Vec<u8>`. Hence
/// [`Reader::try_slice`], which offers the borrow and declines rather than
/// requiring it. What this settles is the offset within the document, which is
/// the half the writer can control.
#[inline]
pub fn size_aligned_after<T: Write + ?Sized>(value: &T, prefix: usize) -> usize {
    size_aligned_after_with::<Standard, T>(value, prefix)
}

/// [`size_aligned_after`] under an explicit [write policy](crate::Options).
#[inline]
pub fn size_aligned_after_with<O: Options, T: Write + ?Sized>(value: &T, prefix: usize) -> usize {
    let mut w = Writer::<Measured<O>>::new().aligned().at(prefix);
    value.write(&mut w);
    w.measured() - prefix
}

/// Parse one BEVE document from an [`io::Read`].
///
/// The reader is drained into memory and then parsed, so this is a convenience
/// rather than a way to bound memory: the encoded document is held whole, and
/// then the value built from it is held beside it.
///
/// Two things bound it instead, and both want the top level to be an array,
/// that being where a document divides. [`Documents::array`] hands out one
/// element at a time however long the array runs, converting a stored width
/// into the one you asked for as it goes. [`read_array_into`] is the same
/// bound without the per-element work: where the stored element type is
/// already `T`'s, the payload goes from the reader into the vector's own
/// memory, which is the multi-gigabyte case. [`Documents::values`] is neither,
/// buffering one whole document exactly as this call does.
///
/// A document whose top level is not an array offers no smaller unit to be
/// read in, and this is the call for it.
///
/// `T` may not borrow from the input, since the buffer does not outlive the
/// call. Use [`from_slice`] over a buffer you keep for the borrowing form.
pub fn from_reader<T, R>(reader: R) -> StreamResult<T>
where
    T: for<'de> Read<'de> + Default,
    R: io::Read,
{
    from_reader_with::<Standard, T, R>(reader)
}

/// [`from_reader`] under an explicit [read policy](crate::Options).
pub fn from_reader_with<O, T, R>(mut reader: R) -> StreamResult<T>
where
    O: Options,
    T: for<'de> Read<'de> + Default,
    R: io::Read,
{
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    from_slice_with::<O, T>(&buf).map_err(StreamError::Parse)
}
