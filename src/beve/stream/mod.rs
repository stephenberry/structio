//! Reading BEVE that does not fit, or has not arrived.
//!
//! [`from_slice`](crate::beve::from_slice) wants the whole document in memory
//! at once, and [`from_reader`](crate::beve::from_reader) only hides the
//! `read_to_end` that puts it there. This module covers the cases where that is
//! not what you have: a file of records too large to hold, bytes arriving in
//! pieces, or a single numeric array too large to hold twice.
//!
//! ```
//! # #[derive(Default, Debug, PartialEq)] struct Sample { t: f64, v: f64 }
//! # structio::object!(Sample { t, v });
//! # let file = structio::to_beve(&vec![Sample { t: 0.0, v: 1.0 }]);
//! let mut docs = structio::beve::Documents::array(&file[..]);
//! for sample in docs.iter::<Sample>() {
//!     let sample = sample?;
//!     // one record resident, whatever the size of the file
//! #   assert_eq!(sample.v, 1.0);
//! }
//! # Ok::<(), structio::StreamError>(())
//! ```
//!
//! [`Documents`] pulls values out of an [`io::Read`](std::io::Read); [`Feed`]
//! is the same machine with the control inverted, for bytes pushed at you.
//! [`Mode`] says how the producer laid the stream out: whole documents one
//! after another, or the elements of one enormous array.
//!
//! [`read_array_into`] is the third case and a different machine: not a stream
//! of values but a single numeric array too large to hold twice, moved from
//! the reader into the destination vector in one direction with nothing
//! staged in between.
//!
//! # What suspends, and what does not
//!
//! Both readers suspend and resume at any byte: the walk that finds a value's
//! extent carries its whole state across chunk boundaries. What they do not do
//! is hand back a half-filled struct. A value is read once its bytes are all
//! present, by the same [`Reader`](crate::beve::Reader) the batch API uses,
//! which is what keeps streamed and slurped documents from ever disagreeing
//! about what a document means, and keeps borrowed `&str` and `&[u8]` fields
//! working.
//!
//! The practical consequence is that memory is bounded by the largest single
//! *value*, not by the largest document. For [`Mode::Array`] that is one
//! element, which is the case a large file exists to serve. A single enormous
//! object read as one value is still buffered whole.
//!
//! # Typed arrays stream too
//!
//! A typed array stores one header for a whole run of elements, so an element
//! cut out of it is not, by itself, a value anything could read. The splitter
//! hands the header the array implied to the reader alongside the span, which
//! is the same trick the array driver plays inside
//! [`Reader::read_seq`](crate::beve::Reader::read_seq) and means a file that is
//! one enormous `Vec<f64>` streams as `f64`s rather than being buffered whole.
//!
//! It does give up the bulk path: read whole, a `Vec<f64>` of a million samples
//! is one `memcpy`, and streamed it is a million reads of eight bytes. That is
//! the price of handing back elements one at a time and of converting a stored
//! width into the one asked for on the way. Where neither is wanted, and the
//! whole document is the array, [`read_array_into`] keeps the block copy and
//! still never holds the encoded form.
//!
//! # Untrusted input
//!
//! There is no size limit by default, because failing on a legitimately large
//! record is worse than the alternative for the common case. When the producer
//! is not trusted, set one with [`Documents::max_value`] or [`Feed::max_value`].
//! BEVE states its own extents, so the shape a hostile document takes here is a
//! length it never delivers, and the limit is what stops the window growing to
//! meet it: past it the read fails with
//! [`ErrorCode::DocumentTooLarge`](crate::ErrorCode::DocumentTooLarge) rather
//! than buffering on.
//!
//! [`read_array_into`] needs no such limit. It buffers nothing beyond the
//! vector it is filling, so a length never delivered costs whatever did
//! arrive and then fails.
//!
//! A value that fails to *read* is reported and skipped, so one bad record in a
//! file does not end the run. A failure to *frame* is terminal: the position in
//! the stream is no longer known, so it is reported once and the stream ends
//! there.
//!
//! One divergence from the batch API is deliberate: an empty input is an empty
//! stream, with zero values and no error, in either mode.
//! [`from_slice`](crate::beve::from_slice) rejects the same input, because there
//! a document was asked for and none was found. An empty file is a normal thing
//! for a stream of records to be.

mod array;
mod feed;
mod read;
mod split;
mod window;

pub use array::{from_reader_array, read_array_into};
pub use feed::Feed;
pub use read::{Documents, Iter};
pub use split::Mode;

use crate::error::{StreamError, StreamResult};
