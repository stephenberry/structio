//! One enormous numeric array, read without holding its encoded form.
//!
//! [`Documents`](super::Documents) bounds memory by handing back one value at a
//! time, and its floor is therefore one whole value. A document that *is* one
//! array has no smaller unit to be read in, which leaves
//! [`from_reader`](crate::beve::from_reader) and its `read_to_end`. That is the
//! case this module covers, and the one shape where it can be covered without a
//! second decoder: a typed numeric array's payload is already the in-memory
//! form of `[T]`, so it can go from the reader into the vector's own memory
//! with nothing in between.

use std::io;

use crate::beve::header;
use crate::beve::impls::NumericBytes;
use crate::error::{Error, ErrorCode, StreamError, StreamResult};

/// Payload bytes taken per read.
///
/// The vector grows toward the stated count in steps of this rather than
/// reserving it all at once, which is what keeps a count read off the wire from
/// deciding how much is allocated. Large enough that a multi-gigabyte array is
/// thousands of reads and not millions.
const CHUNK: usize = 1 << 20;

/// Read a document that is one numeric array from an [`io::Read`], into `out`.
///
/// [`from_reader`](crate::beve::from_reader) drains the reader into a buffer
/// and parses that, so a gigabyte of `f64` is resident twice at the moment it
/// finishes: once encoded, once as the `Vec`. Here the payload goes from the
/// reader straight into the vector's own memory, so the vector is the only
/// thing held. Growing it is still a `Vec` growing, so a reallocation the
/// allocator cannot satisfy in place holds the old buffer and the new one at
/// once; reading into a vector that already has the capacity avoids even
/// that.
///
/// Both typed array forms are accepted, [plain](crate::beve::to_vec) and
/// [aligned](crate::beve::to_vec_aligned). The padding the aligned form carries
/// exists so a reader can point at the payload rather than copy it; there is
/// nothing to point at in a stream, so it is stepped over. A *generic* array is
/// [`ExpectedArray`](ErrorCode::ExpectedArray) even where its elements are all
/// numbers, holding a header apiece rather than a block, and
/// [`from_slice`](crate::beve::from_slice) reads one into a `Vec<T>` where this
/// will not.
///
/// `out` is cleared first and keeps its capacity, which is what the `_into`
/// form is for: a pull loop reading one array after another allocates once
/// rather than holding the old vector alive while it builds the next.
/// [`from_reader_array`] is the same read into a fresh vector.
///
/// The array has to be the whole document; bytes after it are
/// [`TrailingContent`](ErrorCode::TrailingContent), as they are for
/// [`from_slice`](crate::beve::from_slice). Confirming that costs one read past
/// the payload, so a reader that will not report end of input blocks here for
/// the same reason `read_to_end` would.
///
/// A stream carrying more than the array has to be bounded to it first, and
/// [`Read::take`](io::Read::take) is the whole of that where the length is
/// known, as it is in a framed protocol: the frame's end is the document's
/// end, and the reader is left standing on the next frame. Where the array
/// comes after other values rather than instead of them,
/// [`Documents::into_parts`](super::Documents::into_parts) hands back the
/// reader together with the bytes it read past them, so chaining the two is a
/// stream that begins at the array. That is the layout to reach for when a
/// payload is a header and a bulk block: written as two values it streams,
/// written as one struct holding the block as a field it does not, since
/// reaching a field means reading the struct and reading the struct means
/// holding the document.
///
/// ```
/// # #[derive(Default, Debug, PartialEq)] struct Header { id: u64 }
/// # structio::object!(Header { id });
/// use std::io::Read as _;
///
/// let mut stream = structio::to_beve(&Header { id: 7 });
/// structio::beve::append(&vec![1.0f64, 2.0], &mut stream);
///
/// let mut docs = structio::beve::Documents::values(&stream[..]);
/// let header: Header = docs.next_value().unwrap()?;
/// let (rest, unread) = docs.into_parts();
///
/// let mut out: Vec<f64> = Vec::new();
/// structio::read_beve_array_into(&mut out, (&unread[..]).chain(rest))?;
///
/// assert_eq!(header.id, 7);
/// assert_eq!(out, [1.0, 2.0]);
/// # Ok::<(), structio::StreamError>(())
/// ```
///
/// # Element type
///
/// The stored element type has to be exactly `T`'s, and anything else is
/// [`ElementTypeMismatch`](ErrorCode::ElementTypeMismatch) rather than a
/// conversion. The width leniency the rest of the crate has is a per-element
/// conversion, and a per-element conversion is the one thing this call exists
/// to avoid. Where the widths really do differ, stream the array with
/// [`Documents::array`](super::Documents::array), which converts an element at
/// a time and is bounded just as tightly.
///
/// Endianness is not a condition, unlike
/// [`Reader::try_slice`](crate::beve::Reader::try_slice): a big-endian host
/// swaps each element in place after the copy. A borrow cannot do that, not
/// owning the bytes; this does.
///
/// # Untrusted input
///
/// The count precedes the payload and need not describe it. Nothing is
/// reserved on its word: the vector doubles toward the count in steps, capped
/// at it, so what is held stays within twice what has arrived plus the chunk
/// being read. A count that overstates the stream therefore fails with
/// [`UnexpectedEnd`](ErrorCode::UnexpectedEnd) having allocated on the order of
/// what was delivered rather than of what was claimed. There is no limit to
/// set, as there is on
/// [`Documents::max_value`](super::Documents::max_value), because there is
/// nothing here for one to protect: the output is the buffer.
///
/// `out` is left empty when the read fails, rather than holding however much
/// arrived before it did.
///
/// ```
/// let doc = structio::to_beve(&vec![1.0f64, 2.0, 3.0]);
///
/// let mut out: Vec<f64> = Vec::new();
/// structio::read_beve_array_into(&mut out, &doc[..])?;
/// assert_eq!(out, [1.0, 2.0, 3.0]);
/// # Ok::<(), structio::StreamError>(())
/// ```
pub fn read_array_into<T, R>(out: &mut Vec<T>, reader: R) -> StreamResult<()>
where
    T: NumericBytes,
    R: io::Read,
{
    let mut src = Source { reader, pos: 0 };
    out.clear();
    let result = src.array_head::<T>().and_then(|n| {
        src.payload(out, n, (CHUNK / size_of::<T>()).max(1))?;
        src.finish()
    });
    if result.is_err() {
        out.clear();
    }
    result
}

/// [`read_array_into`] into a vector of its own.
///
/// Reading into one you keep is what bounds a loop's peak at a single array;
/// this is for the one-shot case.
///
/// ```
/// let doc = structio::to_beve_aligned(&vec![1u32, 2, 3]);
/// let xs: Vec<u32> = structio::from_beve_reader_array(&doc[..])?;
/// assert_eq!(xs, [1, 2, 3]);
/// # Ok::<(), structio::StreamError>(())
/// ```
pub fn from_reader_array<T, R>(reader: R) -> StreamResult<Vec<T>>
where
    T: NumericBytes,
    R: io::Read,
{
    let mut out = Vec::new();
    read_array_into(&mut out, reader)?;
    Ok(out)
}

/// A reader plus the offset into it, so a failure can be located the way one
/// from [`Reader`](crate::beve::Reader) is.
struct Source<R> {
    reader: R,
    pos: usize,
}

impl<R: io::Read> Source<R> {
    fn fail(&self, at: usize, code: ErrorCode) -> StreamError {
        StreamError::Parse(Error::new(code, at))
    }

    /// Fill `buf` exactly, turning a short stream into a parse failure rather
    /// than an I/O one: the bytes ran out, which is a statement about the
    /// document.
    fn exact(&mut self, buf: &mut [u8]) -> StreamResult<()> {
        match self.reader.read_exact(buf) {
            Ok(()) => {
                self.pos += buf.len();
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                Err(self.fail(self.pos, ErrorCode::UnexpectedEnd))
            }
            Err(e) => Err(StreamError::Io(e)),
        }
    }

    fn byte(&mut self) -> StreamResult<u8> {
        let mut b = [0u8; 1];
        self.exact(&mut b)?;
        Ok(b[0])
    }

    /// A compressed size, as [`header::decode_size`] reads it from a slice.
    ///
    /// The width comes from [`header::size_extra`] rather than from a second
    /// copy of the table, so a stream and a slice cannot come to different
    /// conclusions about where a value ends.
    fn size(&mut self) -> StreamResult<u64> {
        let b0 = self.byte()?;
        let mut rest = [0u8; 7];
        let rest = &mut rest[..header::size_extra(b0)];
        self.exact(rest)?;
        let mut v = u64::from(b0 >> 2);
        for (k, &b) in rest.iter().enumerate() {
            v |= u64::from(b) << (6 + 8 * k);
        }
        Ok(v)
    }

    /// A compressed size as a count of elements.
    fn count(&mut self) -> StreamResult<usize> {
        let at = self.pos;
        let n = self.size()?;
        // A count wider than the address space names a payload no stream could
        // deliver, which is the same thing running out of bytes says.
        usize::try_from(n).map_err(|_| self.fail(at, ErrorCode::UnexpectedEnd))
    }

    /// Consume the array's preamble and report how many elements follow.
    ///
    /// The two forms are the ones
    /// [`Reader::try_slice`](crate::beve::Reader::try_slice) borrows from, read
    /// here from a stream rather than a slice: a plain typed array is `HEADER |
    /// SIZE | DATA`, and the aligned form is `HEADER | NUMERIC_HEADER | SIZE |
    /// PADDING_LENGTH | PADDING | DATA`.
    fn array_head<T: NumericBytes>(&mut self) -> StreamResult<usize> {
        let start = self.pos;
        let h = self.byte()?;
        if header::ty(h) != header::TY_TYPED_ARRAY {
            return Err(self.fail(start, ErrorCode::ExpectedArray));
        }
        let (elem, n) = match (header::sub(h), header::count(h)) {
            // Booleans and strings are typed arrays of something that is not a
            // numeric block at all: packed one per bit, and lengths followed by
            // text. They are the wrong element type rather than a bad header.
            (header::CAT_OTHER, header::OTHER_BOOL | header::OTHER_STRING) => {
                return Err(self.fail(start, ErrorCode::ElementTypeMismatch));
            }
            // The aligned form states its element type in a second header and
            // pads the payload so a reader could point straight at it. There is
            // nothing to point at in a stream, so the padding is stepped over.
            (header::CAT_OTHER, header::OTHER_ALIGNED) => {
                let at = self.pos;
                let inner = self.byte()?;
                // The form wraps a *numeric* typed array, and a width settles
                // both halves of that: no category without one gets a width,
                // so the lookup rules out the bool and string arrays as well
                // as the undefined numeric widths.
                if header::ty(inner) != header::TY_TYPED_ARRAY
                    || header::byte_width(header::sub(inner), header::count(inner)).is_none()
                {
                    return Err(self.fail(at, ErrorCode::InvalidHeader));
                }
                let n = self.count()?;
                let pad = usize::from(self.byte()?);
                let mut skip = [0u8; 255];
                self.exact(&mut skip[..pad])?;
                (header::element_of(inner), n)
            }
            // No other byte count is defined under that category.
            (header::CAT_OTHER, _) => return Err(self.fail(start, ErrorCode::InvalidHeader)),
            // A width the format does not define is a malformed header, and
            // saying the element type is wrong instead would send the caller to
            // a slower reader that fails on the same byte.
            (cat, count) if header::byte_width(cat, count).is_none() => {
                return Err(self.fail(start, ErrorCode::InvalidHeader));
            }
            _ => (header::element_of(h), self.count()?),
        };
        if elem != T::ELEMENT {
            return Err(self.fail(start, ErrorCode::ElementTypeMismatch));
        }
        Ok(n)
    }

    /// Fill `out` with `n` elements, `per` of them at a time.
    ///
    /// The chunk is a parameter so the growth arithmetic and the seam between
    /// chunks can be driven with a tiny one in the tests below, where Miri can
    /// afford to watch them.
    fn payload<T: NumericBytes>(
        &mut self,
        out: &mut Vec<T>,
        n: usize,
        per: usize,
    ) -> StreamResult<()> {
        let width = size_of::<T>();
        while out.len() < n {
            let base = out.len();
            let end = n.min(base + per);
            if out.capacity() < end {
                // Double what is already there rather than reserving the count,
                // which the stream has not yet backed with bytes: what is held
                // stays within about twice what has arrived. Capping at `n` is
                // what makes the last step land on exactly the capacity the
                // array needs instead of overshooting past it.
                let want = n.min((out.capacity() * 2).max(end));
                out.reserve_exact(want - base);
            }
            // SAFETY: capacity is at least `end` by the branch above, so there
            // is room for `end - base` more elements past `base`. Zero is a
            // value of every `NumericBytes` type, so this leaves the new tail
            // initialized, which is what makes the byte slice below a reference
            // to initialized memory rather than to uninitialized. Doing it
            // before `set_len` is also what keeps a panicking `Read` impl from
            // exposing the untouched tail.
            unsafe {
                core::ptr::write_bytes(out.as_mut_ptr().add(base), 0u8, end - base);
                out.set_len(end);
            }
            // SAFETY: `out[base..end]` is `end - base` values of `T`, which by
            // the `NumericBytes` contract occupy that many `width`-byte runs of
            // initialized memory with no padding, and `out` is not touched
            // while this borrow of its buffer is alive.
            let dst = unsafe {
                core::slice::from_raw_parts_mut(
                    out.as_mut_ptr().add(base).cast::<u8>(),
                    (end - base) * width,
                )
            };
            self.exact(dst)?;
            // BEVE is little endian, and every bit pattern is a value, so the
            // whole conversion on a big-endian host is reversing each element's
            // bytes where they lie. This is what a borrow cannot do.
            if cfg!(target_endian = "big") && width > 1 {
                for e in dst.chunks_exact_mut(width) {
                    e.reverse();
                }
            }
        }
        Ok(())
    }

    /// Confirm the array was the whole document.
    ///
    /// Straight to the reader rather than through [`Self::exact`], end of input
    /// being the outcome wanted here rather than a failure.
    fn finish(&mut self) -> StreamResult<()> {
        let mut b = [0u8; 1];
        match self.reader.read_exact(&mut b) {
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(()),
            Ok(()) => Err(self.fail(self.pos, ErrorCode::TrailingContent)),
            Err(e) => Err(StreamError::Io(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(values: &[f64]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    /// The growth arithmetic and the seam between chunks, at a chunk small
    /// enough for Miri to walk every step of.
    ///
    /// [`read_array_into`] takes a megabyte at a time, so any test that reaches
    /// a second chunk through the public call is far too large to run under
    /// Miri. That would leave the reserve the `set_len` above rests on watched
    /// only at `base == 0`, which is the one case where it cannot be wrong.
    #[test]
    fn the_chunk_seam_and_the_reserve_it_rests_on() {
        for n in 0..40usize {
            let values: Vec<f64> = (0..n).map(|i| i as f64).collect();
            let bytes = block(&values);
            let mut src = Source {
                reader: &bytes[..],
                pos: 0,
            };
            let mut out: Vec<f64> = Vec::new();
            src.payload(&mut out, n, 3).unwrap();

            assert_eq!(out, values, "n = {n}");
            // Capped at the count, so the last step lands exactly on it rather
            // than on the doubling above it.
            assert_eq!(out.capacity(), n, "n = {n}");
        }
    }

    /// A payload that stops short fails in whichever chunk it stops in, and
    /// never reads past what arrived.
    #[test]
    fn a_short_payload_fails_in_whichever_chunk_it_stops_in() {
        let values: Vec<f64> = (0..20).map(|i| i as f64).collect();
        let bytes = block(&values);

        for cut in 0..bytes.len() {
            let mut src = Source {
                reader: &bytes[..cut],
                pos: 0,
            };
            let mut out: Vec<f64> = Vec::new();
            let err = src.payload(&mut out, 20, 3).unwrap_err();
            assert_eq!(
                err.as_parse().unwrap().code,
                ErrorCode::UnexpectedEnd,
                "cut at {cut}"
            );
        }
    }

    /// A count no stream could deliver reserves on what arrives, not on what
    /// it claims. At `per = 3` the ceiling is small enough to state exactly.
    #[test]
    fn a_lying_count_reserves_on_what_arrived() {
        let values: Vec<f64> = (0..10).map(|i| i as f64).collect();
        let bytes = block(&values);
        let mut src = Source {
            reader: &bytes[..],
            pos: 0,
        };
        let mut out: Vec<f64> = Vec::new();
        assert!(src.payload(&mut out, usize::MAX, 3).is_err());

        // Ten elements arrived; doubling from the last chunk boundary cannot
        // have reached past twice that plus one chunk.
        assert!(out.capacity() <= 23, "capacity {}", out.capacity());
    }
}
