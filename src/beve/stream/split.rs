//! Finding one BEVE value's bytes in a partially arrived stream.
//!
//! Everything on the streaming read side rests on this: a walk that can stop
//! anywhere, including in the middle of a value, and pick up where it left off
//! when more bytes turn up. It never decodes a payload. It only answers "where
//! does the next item end", and hands that span to the ordinary [`Reader`], so
//! a streamed document and a slurped one go down exactly the same code path and
//! cannot disagree about what BEVE means.
//!
//! # Why this is a walk and not a scan
//!
//! JSON's splitter searches for a byte: the boundary is hidden by nesting and
//! by strings, so it tracks depth and quoting and looks at every byte of the
//! input. BEVE states every extent up front, so nothing here looks at a payload
//! at all. What it reads is headers, counts, and object keys; everything else
//! is a number of bytes to step over.
//!
//! That is also what makes suspending harder rather than easier. A scan can
//! stop between any two bytes and resume with a few flags. A walk has to stop
//! part way through a *stated* extent, so it carries two things across the
//! boundary: the containers it is inside ([`Frame`]), and how much of the
//! current payload is still owed ([`Scanner::pending`]). Carrying the
//! remainder, rather than the whole extent, is what keeps a value spread over a
//! thousand chunks from being re-walked a thousand times.
//!
//! # The preamble is decoded by a real [`Reader`]
//!
//! A header and the sizes behind it can straddle a chunk boundary, so the walk
//! must be able to read one and find it incomplete without having consumed
//! anything. A throwaway `Reader` over `buf[pos..]` gives exactly that: on
//! success its own position says how far to advance, and on failure the walk's
//! cursor has not moved. It also means the extent of a value is worked out by
//! the same code that reads and skips one, rather than by a second opinion.

use crate::beve::header::{self, byte_width, decode_size};
use crate::beve::reader::{MAX_DEPTH, Reader, Typed, complex_payload, key_width, payload_len};
use crate::error::{ErrorCode, PResult};
use crate::stream::{Framer, Split};

/// How a byte stream is divided into BEVE values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Mode {
    /// Whole BEVE documents, one after another.
    ///
    /// Nothing separates them: every value states its own extent, so where one
    /// ends the next begins. The specification's delimiter extension may
    /// appear between documents all the same, and is stepped over, so a
    /// producer that writes one is read the same as one that does not.
    Values,
    /// The elements of one top-level array.
    ///
    /// The array's own header and count are consumed by the splitter and each
    /// element is one item. A typed array works here too: its elements carry no
    /// headers of their own, and the header the array implied is supplied to
    /// the reader alongside the span.
    Array,
}

/// A container the walk is partway through.
///
/// Only containers that hold *values* need one. A string, a number, a packed
/// boolean run and a fixed-width numeric block are all a count of bytes to step
/// over, which is [`Scanner::pending`] and not a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Frame {
    /// An object with `left` members still owed. `key` says whether the next
    /// thing due is a key rather than the value that follows it.
    Object {
        /// The key kind, which is the object header's sub-field.
        cat: u8,
        /// Bytes an integer key occupies, or zero for the string form.
        width: usize,
        left: usize,
        key: bool,
    },
    /// `left` whole values still owed: a generic array's elements, or the
    /// operands of an extension that wraps values.
    Values { left: usize },
    /// `left` elements of a typed string array, each a size and its bytes.
    Strings { left: usize },
}

impl Frame {
    /// Are this container's obligations all met?
    ///
    /// An object needs no separate test for standing between a key and its
    /// value: the count falls when the value *begins*, so a frame is never seen
    /// with none left and a value still owed.
    fn done(&self) -> bool {
        match *self {
            Frame::Object { left, .. } | Frame::Values { left } | Frame::Strings { left } => {
                left == 0
            }
        }
    }
}

/// What the walk owes next.
#[derive(Debug, Clone, Copy)]
enum Duty {
    /// A whole value, with a header of its own.
    Value,
    /// An object key of the given kind.
    Key { cat: u8, width: usize },
    /// One element of a typed string array: a size and its bytes.
    Text,
}

/// A resumable walk over one BEVE value.
struct Scanner {
    /// Where the next [`Scanner::advance`] resumes.
    pos: usize,
    /// Raw bytes of the current payload still to step over.
    pending: usize,
    stack: Vec<Frame>,
    /// The root value has been started. Without this an empty stack would read
    /// as "finished" before anything had begun.
    started: bool,
}

impl Scanner {
    fn new() -> Self {
        Scanner {
            pos: 0,
            pending: 0,
            stack: Vec::new(),
            started: false,
        }
    }

    /// Begin a fresh value at `at`.
    fn restart(&mut self, at: usize) {
        self.pos = at;
        self.pending = 0;
        // Kept rather than freed: a stream of records reaches the same depth
        // over and over, and this is the only allocation the walk makes.
        self.stack.clear();
        self.started = false;
    }

    /// Account for the consumed prefix having been removed from the front.
    fn rebase(&mut self, shift: usize) {
        self.pos = self.pos.saturating_sub(shift);
    }

    /// What the walk owes next, without touching anything.
    fn duty(&self) -> Duty {
        match self.stack.last() {
            Some(&Frame::Object {
                cat, width, key, ..
            }) if key => Duty::Key { cat, width },
            Some(Frame::Strings { .. }) => Duty::Text,
            _ => Duty::Value,
        }
    }

    /// Mark the current duty taken.
    ///
    /// Called the moment the walk commits to it, which for a value is before
    /// the value has been walked. That is deliberate: the frames the value
    /// pushes sit above this one, so it will not be consulted again until they
    /// are gone, and by then its count is already right.
    fn taken(&mut self) {
        match self.stack.last_mut() {
            None => self.started = true,
            Some(Frame::Object { left, key, .. }) => {
                if *key {
                    *key = false;
                } else {
                    *left -= 1;
                    *key = true;
                }
            }
            Some(Frame::Values { left } | Frame::Strings { left }) => *left -= 1,
        }
    }

    /// Walk from where the last call stopped to the end of `buf`.
    ///
    /// `Ok(Some(end))` means a complete value occupies everything up to `end`.
    /// `Ok(None)` means more bytes are needed, which is only a failure once the
    /// caller knows none are coming.
    fn advance(&mut self, buf: &[u8]) -> PResult<Option<usize>> {
        loop {
            if self.pending > 0 {
                let take = self.pending.min(buf.len() - self.pos);
                self.pos += take;
                self.pending -= take;
                if self.pending > 0 {
                    return Ok(None);
                }
            }

            while self.stack.last().is_some_and(Frame::done) {
                self.stack.pop();
            }
            if self.started && self.stack.is_empty() {
                return Ok(Some(self.pos));
            }

            match self.duty() {
                // A string key and a string element are the same two fields.
                Duty::Key {
                    cat: header::CAT_FLOAT,
                    ..
                }
                | Duty::Text => {
                    let mut p = self.pos;
                    let Some(n) = size_at(buf, &mut p) else {
                        return Ok(None);
                    };
                    self.pos = p;
                    self.pending = usize::try_from(n).map_err(|_| ErrorCode::UnexpectedEnd)?;
                    self.taken();
                }
                Duty::Key { width, .. } => {
                    if buf.len() - self.pos < width {
                        return Ok(None);
                    }
                    self.pos += width;
                    self.taken();
                }
                Duty::Value => {
                    let (used, skip, frame) = match head(buf, self.pos, self.stack.len()) {
                        Ok(step) => step,
                        // Every short read inside a preamble surfaces as this,
                        // and so does an extent too large to be an extent. The
                        // two are told apart by the caller, which knows whether
                        // more bytes can still arrive.
                        Err(ErrorCode::UnexpectedEnd) => return Ok(None),
                        Err(e) => return Err(e),
                    };
                    self.pos += used;
                    self.pending = skip;
                    self.taken();
                    if let Some(frame) = frame {
                        self.stack.push(frame);
                    }
                }
            }
        }
    }
}

/// Decode a compressed size, or report that its bytes have not all arrived.
///
/// [`decode_size`] has exactly one failure, a short read, and leaves `p` alone
/// when it happens, which is what makes discarding the code sound here.
fn size_at(buf: &[u8], p: &mut usize) -> Option<u64> {
    decode_size(buf, p).ok()
}

/// Read the preamble of the value at `at`: everything up to its payload.
///
/// Reports how many bytes the preamble occupied, how many bytes of payload
/// follow it, and the container it opens if it opens one. A container that
/// holds no values -- a packed boolean run, a fixed-width block -- opens none
/// and states its whole payload instead.
///
/// `depth` is how many containers are already open, and is charged exactly as
/// [`Reader::skip_value`] charges it, typed arrays included. Doing otherwise in
/// either direction would frame documents the reader will not read, or refuse
/// documents it would.
fn head(buf: &[u8], at: usize, depth: usize) -> PResult<(usize, usize, Option<Frame>)> {
    let mut r = Reader::new(&buf[at..]);
    let h = r.head()?;
    let (skip, frame) = match header::ty(h) {
        // Null and the booleans are the header and nothing else. Only three of
        // the four sub-codes are defined, and the byte-count field must be
        // zero, so the rest are not values to step over.
        header::TY_NULL_BOOL => match h {
            header::NULL | header::FALSE | header::TRUE => (0, None),
            _ => return Err(ErrorCode::InvalidHeader),
        },
        header::TY_NUMBER => {
            let w = byte_width(header::sub(h), header::count(h)).ok_or(ErrorCode::InvalidHeader)?;
            (w, None)
        }
        header::TY_STRING => (r.count()?, None),
        header::TY_OBJECT => {
            let width = key_width(h)?;
            let left = r.count()?;
            enter(depth)?;
            let cat = header::sub(h);
            (
                0,
                Some(Frame::Object {
                    cat,
                    width,
                    left,
                    key: true,
                }),
            )
        }
        header::TY_GENERIC_ARRAY => {
            let left = r.count()?;
            enter(depth)?;
            (0, Some(Frame::Values { left }))
        }
        header::TY_TYPED_ARRAY => {
            let form = r.typed_head(h)?;
            enter(depth)?;
            match form {
                Typed::Bools(n) => (n.div_ceil(8), None),
                Typed::Strings(n) => (0, Some(Frame::Strings { left: n })),
                Typed::Fixed(elem, n) => (payload_len(elem, n)?, None),
            }
        }
        header::TY_EXTENSION => match header::ext_id(h) {
            header::EXT_DELIMITER => (0, None),
            // The deprecated type tag: an index, then the value it tagged.
            header::EXT_TYPE_TAG => {
                r.size()?;
                enter(depth)?;
                (0, Some(Frame::Values { left: 1 }))
            }
            // A layout byte, then the extents and the data, both typed arrays.
            // The layout byte is payload rather than preamble, so it is stepped
            // over before the frame's first value comes due.
            header::EXT_MATRIX => {
                enter(depth)?;
                (1, Some(Frame::Values { left: 2 }))
            }
            // Straight through the reader's own walk. This module's whole
            // argument is that an extent comes from the code that reads and
            // skips a value rather than from a second opinion, and a complex
            // value is no exception now that the reader has the helper.
            header::EXT_COMPLEX => {
                let (_, width, pairs) = r.complex_head()?;
                (complex_payload(width, pairs)?, None)
            }
            _ => return Err(ErrorCode::UnsupportedFeature),
        },
        _ => return Err(ErrorCode::InvalidHeader),
    };
    Ok((r.position(), skip, frame))
}

/// Charge a level for a container about to be opened.
fn enter(depth: usize) -> PResult<()> {
    if depth >= MAX_DEPTH as usize {
        return Err(ErrorCode::ExceededMaxDepth);
    }
    Ok(())
}

/// How the elements of a top-level array are laid out, in [`Mode::Array`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Elem {
    /// A generic array: each element is a whole value, walked like any other.
    Generic,
    /// A fixed-width block: each element is `width` bytes under `implied`.
    Fixed { implied: u8, width: usize },
    /// A run of strings: each element is a size and its bytes.
    Strings,
    /// A packed boolean run: each element is a bit, and no bytes at all. `bit`
    /// is the offset within the byte at the cursor, so the cursor keeps up with
    /// the run rather than pinning its whole payload.
    Bools { bit: u32 },
}

/// Where the splitter is between items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// [`Mode::Values`]: at a document boundary.
    Between,
    /// Walking one value: a whole document, or one element of a generic array.
    InItem,
    /// [`Mode::Array`]: the top-level array's own header is still to be read.
    BeforeArray,
    /// [`Mode::Array`]: between elements.
    BeforeElement,
    /// The stream is over; nothing may follow.
    Done,
}

/// Divides a growing byte buffer into BEVE values.
///
/// The [`Framer`] impl below is the whole interface; see it for what the
/// window may discard and how it says so.
pub(crate) struct Splitter {
    mode: Mode,
    scan: Scanner,
    state: State,
    /// First byte not yet accounted for. Everything below it is dead.
    cursor: usize,
    /// [`Mode::Array`]: elements the top-level array still owes.
    left: usize,
    /// [`Mode::Array`]: how those elements are stored.
    elem: Elem,
    /// The header the item just produced was implied to carry, if any.
    implied: Option<u8>,
}

impl Splitter {
    pub(crate) fn new(mode: Mode) -> Self {
        Splitter {
            mode,
            scan: Scanner::new(),
            state: match mode {
                Mode::Array => State::BeforeArray,
                Mode::Values => State::Between,
            },
            cursor: 0,
            left: 0,
            elem: Elem::Generic,
            implied: None,
        }
    }

    /// The header the item just produced carries but does not contain.
    ///
    /// `Some` only for the elements of a typed array, which is the one place
    /// BEVE stores a value without a header of its own.
    pub(crate) fn implied(&self) -> Option<u8> {
        self.implied
    }

    /// Take the top-level array's header and learn how its elements are laid
    /// out.
    ///
    /// Decoded by a throwaway [`Reader`] for the same reason the walk's
    /// preamble is: the aligned form's second header and padding are fiddly
    /// enough that a second opinion about them would eventually be a different
    /// opinion.
    fn open_array(&mut self, buf: &[u8], eof: bool) -> PResult<Option<Split>> {
        if self.cursor >= buf.len() {
            // Nothing at all, and nothing more coming, is an empty stream
            // rather than a malformed array.
            return Ok(Some(if eof { Split::End } else { Split::Need }));
        }
        let mut r = Reader::new(&buf[self.cursor..]);
        let outcome = (|| {
            let h = r.head()?;
            match header::ty(h) {
                header::TY_GENERIC_ARRAY => Ok((r.count()?, Elem::Generic)),
                header::TY_TYPED_ARRAY => Ok(match r.typed_head(h)? {
                    Typed::Bools(n) => (n, Elem::Bools { bit: 0 }),
                    Typed::Strings(n) => (n, Elem::Strings),
                    Typed::Fixed(elem, n) => (
                        n,
                        Elem::Fixed {
                            implied: header::element_of(elem),
                            width: byte_width(header::sub(elem), header::count(elem))
                                .ok_or(ErrorCode::InvalidHeader)?,
                        },
                    ),
                }),
                // A complex array is a sequence too, and the synthetic
                // element header is exactly the one byte `with_implied` wants,
                // so it needs nothing here that a typed array does not.
                header::TY_EXTENSION if h == header::COMPLEX => {
                    let (class, width, pairs) = r.complex_head()?;
                    let n = pairs.ok_or(ErrorCode::ExpectedArray)?;
                    Ok((
                        n,
                        Elem::Fixed {
                            implied: header::complex_element(class),
                            // Two components to an element.
                            width: 2 * width,
                        },
                    ))
                }
                _ => Err(ErrorCode::ExpectedArray),
            }
        })();
        let (left, elem) = match outcome {
            Ok(pair) => pair,
            Err(ErrorCode::UnexpectedEnd) if !eof => return Ok(Some(Split::Need)),
            Err(e) => return Err(e),
        };
        self.cursor += r.position();
        self.left = left;
        self.elem = elem;
        self.state = State::BeforeElement;
        Ok(None)
    }

    /// Step the cursor past whatever tail the last element left behind.
    fn close_array(&mut self) {
        // A packed run ends mid-byte unless its length was a multiple of eight,
        // and that last byte is still live.
        if let Elem::Bools { bit } = self.elem
            && bit > 0
        {
            self.cursor += 1;
            self.elem = Elem::Bools { bit: 0 };
        }
        self.state = State::Done;
    }
}

impl Framer for Splitter {
    fn next(&mut self, buf: &[u8], eof: bool) -> PResult<Split> {
        self.implied = None;
        loop {
            match self.state {
                State::Between => {
                    // A delimiter between documents is a separator rather than
                    // a value, and a trailing one ends the stream as cleanly as
                    // no delimiter at all.
                    while buf.get(self.cursor) == Some(&header::DELIMITER) {
                        self.cursor += 1;
                    }
                    if self.cursor >= buf.len() {
                        return Ok(if eof { Split::End } else { Split::Need });
                    }
                    self.scan.restart(self.cursor);
                    self.state = State::InItem;
                }

                State::InItem => {
                    let Some(end) = self.scan.advance(buf)? else {
                        return if eof {
                            Err(ErrorCode::UnexpectedEnd)
                        } else {
                            Ok(Split::Need)
                        };
                    };
                    let start = self.cursor;
                    self.cursor = end;
                    match self.mode {
                        Mode::Values => self.state = State::Between,
                        Mode::Array => {
                            self.left -= 1;
                            self.state = State::BeforeElement;
                        }
                    }
                    return Ok(Split::Item { start, end });
                }

                State::BeforeArray => {
                    if let Some(split) = self.open_array(buf, eof)? {
                        return Ok(split);
                    }
                }

                // One element, in whichever of the four shapes it is stored.
                // Each advances the cursor past what it produced, so the window
                // holds one element and never the array.
                State::BeforeElement => {
                    if self.left == 0 {
                        self.close_array();
                        continue;
                    }
                    let start = self.cursor;
                    let found = match &mut self.elem {
                        Elem::Generic => {
                            if start < buf.len() {
                                self.scan.restart(start);
                                self.state = State::InItem;
                                continue;
                            }
                            None
                        }
                        Elem::Fixed { implied, width } => {
                            let (implied, width) = (*implied, *width);
                            if buf.len() - start < width {
                                None
                            } else {
                                self.cursor = start + width;
                                self.implied = Some(implied);
                                Some(start + width)
                            }
                        }
                        Elem::Strings => {
                            let mut p = start;
                            match size_at(buf, &mut p)
                                .and_then(|n| usize::try_from(n).ok())
                                .and_then(|n| p.checked_add(n))
                                .filter(|&end| end <= buf.len())
                            {
                                Some(end) => {
                                    self.cursor = end;
                                    self.implied = Some(header::STRING);
                                    Some(end)
                                }
                                None => None,
                            }
                        }
                        Elem::Bools { bit } => match buf.get(start) {
                            Some(&byte) => {
                                self.implied = Some(if (byte >> *bit) & 1 == 1 {
                                    header::TRUE
                                } else {
                                    header::FALSE
                                });
                                *bit += 1;
                                if *bit == 8 {
                                    *bit = 0;
                                    self.cursor = start + 1;
                                }
                                // A packed boolean is its header and nothing
                                // else, so the span is empty and the bit that
                                // decided it rode out in `implied`.
                                Some(start)
                            }
                            None => None,
                        },
                    };
                    return match found {
                        Some(end) => {
                            self.left -= 1;
                            Ok(Split::Item { start, end })
                        }
                        None if eof => Err(ErrorCode::UnexpectedEnd),
                        None => Ok(Split::Need),
                    };
                }

                State::Done => {
                    // Everything asked for has been produced, so any byte still
                    // here belongs to nothing.
                    return if self.cursor < buf.len() {
                        Err(ErrorCode::TrailingContent)
                    } else if eof {
                        Ok(Split::End)
                    } else {
                        Ok(Split::Need)
                    };
                }
            }
        }
    }

    #[inline]
    fn consumed(&self) -> usize {
        self.cursor
    }

    fn rebase(&mut self) {
        let shift = self.cursor;
        self.cursor = 0;
        self.scan.rebase(shift);
    }

    /// The walk stops its own position on the offending byte and leaves the
    /// cursor at the start of the item; the states outside it move the cursor
    /// instead. Whichever is further along is the one that just moved.
    #[inline]
    fn position(&self) -> usize {
        self.cursor.max(self.scan.pos)
    }
}
