//! Reading and writing JSON that does not fit, or has not arrived.
//!
//! The batch entry points ([`from_str`](crate::from_str),
//! [`to_string`](crate::to_string)) want the whole document in memory at once.
//! This module covers the cases where that is not what you have: output going
//! to a socket, input arriving in pieces, or a file of records too large to
//! hold.
//!
//! # Writing
//!
//! [`to_writer`] serializes straight into an [`io::Write`], draining as it
//! goes. Peak memory is the writer's buffer plus the largest single scalar,
//! whatever the size of the document.
//!
//! ```no_run
//! # #[derive(Default)] struct Record { id: u64 }
//! # structio::object!(Record { id });
//! # fn main() -> std::io::Result<()> {
//! let file = std::fs::File::create("out.json")?;
//! structio::to_writer(&Record { id: 7 }, file)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Reading a sequence
//!
//! [`Documents`] turns a reader into a series of values, holding only one at a
//! time. It handles the three shapes a JSON stream comes in: newline-delimited
//! records, the elements of one large array, and bare values back to back.
//!
//! ```
//! # #[derive(Default, Debug, PartialEq)] struct Record { id: u64 }
//! # structio::object!(Record { id });
//! let input = b"{\"id\":1}\n{\"id\":2}\n" as &[u8];
//! let mut docs = structio::Documents::lines(input);
//! let ids: Vec<u64> = docs
//!     .iter::<Record>()
//!     .map(|r| r.unwrap().id)
//!     .collect();
//! assert_eq!(ids, [1, 2]);
//! ```
//!
//! Values that borrow from the input are available too, through
//! [`Documents::next_value`], which holds the reader still for as long as the value
//! lives.
//!
//! # Reading from chunks you are handed
//!
//! [`Feed`] is the same machine driven from the other side: push bytes in as
//! they arrive, and take values out as they complete. Chunks may split a value
//! anywhere, including inside a string or a number.
//!
//! ```
//! # #[derive(Default, Debug, PartialEq)] struct Record { id: u64 }
//! # structio::object!(Record { id });
//! let mut feed = structio::Feed::values();
//! feed.push(b"{\"id\":1}{\"i");
//! assert_eq!(feed.next_value::<Record>().unwrap().unwrap(), Record { id: 1 });
//! assert!(feed.next_value::<Record>().is_none());
//! feed.push(b"d\":2}");
//! assert_eq!(feed.next_value::<Record>().unwrap().unwrap(), Record { id: 2 });
//! ```
//!
//! # What suspends, and what does not
//!
//! Both readers suspend and resume at any byte: the structural scan that finds
//! a value's extent carries its whole state across chunk boundaries. What they
//! do not do is hand back a half-filled struct. A value is parsed once its
//! bytes are all present, by the same [`Parser`](crate::json::Parser) the batch API
//! uses, which is what keeps streamed and slurped documents from ever
//! disagreeing about what a document means, and keeps borrowed `&str` fields
//! working.
//!
//! The practical consequence is that memory is bounded by the largest single
//! *value*, not by the largest document. For [`Mode::Array`] and
//! [`Mode::Lines`] that is one record, which is the case the format exists to
//! serve. A single enormous object read as one value is still buffered whole.
//!
//! # Untrusted input
//!
//! There is no size limit by default, because failing on a legitimately large
//! record is worse than the alternative for the common case. When the producer
//! is not trusted, set one with [`Documents::max_value`] or
//! [`Feed::max_value`]; a value that exceeds it fails with
//! [`ErrorCode::DocumentTooLarge`](crate::ErrorCode::DocumentTooLarge) rather
//! than growing the buffer. On the pull side reads are clipped so the window
//! never runs more than a byte past the limit; on the push side the limit
//! bounds what is retained, not the size of a chunk you hand to
//! [`Feed::push`].
//!
//! A value that fails to *parse* is reported and skipped, so one bad record in
//! a file does not end the run. A failure to *frame* is terminal: the position
//! in the input is no longer known, so it is reported once and the stream ends
//! there.
//!
//! One divergence from the batch API is deliberate: an input holding nothing
//! but whitespace is an empty stream, with zero values and no error, in every
//! mode. [`from_str`](crate::from_str) rejects the same input, because there a
//! document was asked for and none was found. An empty file is a normal thing
//! for a stream of records to be.

use std::io;

use crate::json::traits::{Read, Write};
use crate::json::writer::Writer;
use crate::options::{Options, Standard};

mod feed;
mod read;
mod split;
mod window;

pub use feed::Feed;
pub use read::{Documents, Iter};
pub use split::Mode;

// Not re-exported: `StreamError` is format independent and lives in
// [`error`](crate::error), reachable as `structio::StreamError`. Naming it here
// too would imply the JSON side owns it, which it has not since BEVE arrived.
use crate::error::{StreamError, StreamResult};

/// Serialize a value straight into an [`io::Write`].
///
/// The document is drained to `out` as it is produced rather than assembled
/// first, so this holds [`DEFAULT_SINK_BUFFER`](crate::json::writer::DEFAULT_SINK_BUFFER)
/// bytes plus the largest single value written, however large the document is.
/// A long string is that largest value and is buffered whole, so the bound is
/// only as good as the longest string in the document.
///
/// Wrapping `out` in a [`BufWriter`](std::io::BufWriter) is redundant: this
/// already buffers and hands the sink whole blocks. `out` is not flushed.
///
/// The error is `out`'s own, handed back unchanged. A [`Write`] implementation
/// returns nothing to fail with, so the sink is the only thing on this path
/// that can go wrong and there is no crate error type to fold it into. A
/// caller wanting one error type across reading and writing has
/// [`StreamError`], which converts from [`io::Error`] with `?`.
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

/// Parse one JSON document from an [`io::Read`].
///
/// The reader is drained into memory and then parsed, so this is a
/// convenience rather than a way to bound memory: the whole document is held
/// at once. For a stream of many values, or for a document larger than you
/// want resident, use [`Documents`].
///
/// `T` may not borrow from the input, since the buffer does not outlive the
/// call. [`Documents::next_value`] is the borrowing form.
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
    crate::json::from_slice_with::<O, T>(&buf).map_err(StreamError::Parse)
}
