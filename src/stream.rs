//! The byte window both formats' streaming readers pull through.
//!
//! Neither streaming reader parses as it reads. Each finds where one value
//! ends and hands that span to the ordinary parser, so a streamed document and
//! a slurped one go down the same code path and cannot disagree about what the
//! format means. What differs between the two is only the search for that
//! boundary, which is the [`Framer`]. Everything around it -- acquiring bytes,
//! keeping the buffer bounded, holding the size limit, placing an error against
//! the whole stream -- is the same problem twice, and is solved here once.

use std::io;

use crate::error::{Error, ErrorCode, PResult, StreamError};

/// Bytes requested per read, and the initial buffer size.
pub(crate) const DEFAULT_BUFFER: usize = 64 * 1024;

/// What the framer could determine from the bytes it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Split {
    /// A complete item occupies `buf[start..end]`.
    Item { start: usize, end: usize },
    /// Not enough bytes yet. More input may produce an item.
    Need,
    /// The stream is finished; there are no more items.
    End,
}

/// Divides a growing byte buffer into values.
///
/// Offsets are into the caller's buffer. The window may discard everything
/// below [`Framer::consumed`] as long as it says so with [`Framer::rebase`].
pub(crate) trait Framer {
    /// The next item, if the bytes for one have arrived.
    ///
    /// `eof` says whether `buf` is all the input there will ever be. Every
    /// state must resolve when it is set: a framer that answers [`Split::Need`]
    /// at end of input would leave the reader waiting for bytes that cannot
    /// come.
    fn next(&mut self, buf: &[u8], eof: bool) -> PResult<Split>;

    /// Bytes the window may discard from the front of its buffer.
    fn consumed(&self) -> usize;

    /// Account for the consumed prefix having been removed from the front.
    ///
    /// Takes no argument because there is only one legal amount to shift by:
    /// everything the framer has finished with, which is exactly `consumed`.
    /// Letting the caller name a different number would be a way to get the
    /// offsets wrong.
    fn rebase(&mut self);

    /// Where a failure was detected.
    fn position(&self) -> usize;
}

/// A growing buffer, a framer over it, and the policy that keeps the two
/// bounded.
pub(crate) struct Window<F> {
    buf: Vec<u8>,
    split: F,
    /// No more bytes will arrive.
    eof: bool,
    /// Largest single value that may be buffered.
    limit: usize,
    /// Bytes dropped off the front, so an error can be located against the
    /// whole stream rather than against the current window.
    origin: usize,
    /// Framing has failed. The position in the stream is no longer known, so
    /// there is nothing to resume from: the window reports the stream over,
    /// and stops accepting bytes so a dead reader cannot still be filled.
    failed: bool,
    /// How far into the buffer's allocation bytes are known to be
    /// initialized. Lets [`Self::fill`] hand `read` a slice it has already
    /// paid to zero once, rather than zeroing a fresh one every time.
    inited: usize,
}

impl<F: Framer> Window<F> {
    pub(crate) fn new(split: F, capacity: usize) -> Self {
        Window {
            buf: Vec::with_capacity(capacity),
            split,
            eof: false,
            limit: usize::MAX,
            origin: 0,
            failed: false,
            inited: 0,
        }
    }

    pub(crate) fn set_limit(&mut self, bytes: usize) {
        self.limit = bytes;
    }

    /// The framer, for whatever it knows about the item it just produced that
    /// a span alone cannot say.
    pub(crate) fn framer(&self) -> &F {
        &self.split
    }

    /// The framer, to configure before any bytes reach it.
    pub(crate) fn framer_mut(&mut self) -> &mut F {
        &mut self.split
    }

    /// The live bytes, which a located span indexes into.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Bytes held but not yet resolved into a value.
    pub(crate) fn buffered(&self) -> usize {
        if self.failed {
            // The buffer was dropped on failure, but the framer's cursor
            // stayed where it stopped, so the subtraction below would wrap.
            return 0;
        }
        self.buf.len() - self.split.consumed()
    }

    /// Take the bytes that were read but never resolved into a value.
    ///
    /// Empty once framing has failed, which is also when the buffer was
    /// dropped: there is no longer a known position for those bytes to be
    /// relative to, so handing them back would be handing back garbage.
    pub(crate) fn into_unread(self) -> Vec<u8> {
        if self.failed {
            return Vec::new();
        }
        let mut buf = self.buf;
        buf.drain(..self.split.consumed());
        buf
    }

    /// Byte offset in the stream of the next unresolved byte.
    pub(crate) fn offset(&self) -> usize {
        self.origin + self.split.consumed()
    }

    pub(crate) fn is_eof(&self) -> bool {
        self.eof
    }

    pub(crate) fn set_eof(&mut self) {
        self.eof = true;
    }

    /// Add bytes to the window.
    ///
    /// Ignored once framing has failed, so a caller that keeps pushing at a
    /// dead stream cannot make it grow. That is the case [`Self::set_limit`]
    /// exists to prevent, and it would otherwise be reachable by pushing past
    /// the error.
    pub(crate) fn extend(&mut self, bytes: &[u8]) {
        if self.failed {
            return;
        }
        self.buf.extend_from_slice(bytes);
        // `extend_from_slice` may reallocate, which leaves the spare capacity
        // of the new allocation uninitialized.
        self.inited = self.buf.len();
    }

    /// Drop the bytes the framer is finished with.
    ///
    /// Only when at least half the window is dead, so a long stream of small
    /// values does not pay a full-window move per value. That keeps the moved
    /// bytes amortized to a constant per byte read, and bounds the window at
    /// roughly twice the live data plus one read.
    ///
    /// Safe to call only when no value parsed out of the buffer is still
    /// borrowed, which is why it happens at the top of [`Self::try_next`].
    fn compact(&mut self) {
        let dead = self.split.consumed();
        if dead == 0 || dead < self.buf.len() - dead {
            return;
        }
        self.buf.copy_within(dead.., 0);
        self.buf.truncate(self.buf.len() - dead);
        self.split.rebase();
        self.origin += dead;
    }

    /// Ask the framer for the next value, without acquiring more input.
    ///
    /// A failure here is a framing failure, and it is terminal: it is reported
    /// once, and afterwards the window reports the stream over. Returning the
    /// same error forever would turn the natural `while let` loop into a spin,
    /// and there is no position to resume from anyway.
    pub(crate) fn try_next(&mut self) -> Result<Split, StreamError> {
        if self.failed {
            return Ok(Split::End);
        }
        match self.locate() {
            Ok(split) => Ok(split),
            Err(e) => {
                self.failed = true;
                // Nothing here can be parsed any more, so do not go on holding
                // it.
                self.buf = Vec::new();
                self.inited = 0;
                Err(e)
            }
        }
    }

    fn locate(&mut self) -> Result<Split, StreamError> {
        self.compact();
        let split = self
            .split
            .next(&self.buf, self.eof)
            .map_err(|code| self.error_at(code, self.split.position()))?;
        // The limit is on one value's extent, however its bytes arrived. A
        // starved framer is holding a partial value, so the whole live window
        // is it; a located one states its own size. Both report the offset of
        // the value's first byte, not of wherever the search happens to be.
        let (extent, at) = match split {
            Split::Need => (self.buffered(), self.split.consumed()),
            Split::Item { start, end } => (end - start, start),
            Split::End => (0, self.split.consumed()),
        };
        if extent > self.limit {
            return Err(self.error_at(ErrorCode::DocumentTooLarge, at));
        }
        Ok(split)
    }

    /// Read one chunk from `reader`.
    ///
    /// A read of zero is taken as end of input, per the [`io::Read`] contract.
    pub(crate) fn fill<R: io::Read>(&mut self, reader: &mut R, chunk: usize) -> io::Result<()> {
        let len = self.buf.len();
        // Never read further past the limit than it takes to notice it. The
        // point of the limit is that a hostile stream cannot grow the window,
        // and reading a whole 64 KiB before looking would make the real bound
        // `limit + 64 KiB`.
        let room = self.limit.saturating_sub(self.buffered()).saturating_add(1);
        let want = len + chunk.clamp(1, room);
        if want > self.inited {
            self.buf.resize(want, 0);
            self.inited = want;
        } else {
            // SAFETY: `want <= inited`, and everything below `inited` was
            // initialized by an earlier `resize` into this same allocation.
            // Nothing between then and now can have replaced it: `compact`
            // moves bytes down within the buffer without reallocating, and
            // `extend` resets `inited` when it may have reallocated.
            unsafe { self.buf.set_len(want) };
        }

        let read = reader.read(&mut self.buf[len..]);
        match read {
            Ok(0) => {
                self.buf.truncate(len);
                self.eof = true;
                Ok(())
            }
            Ok(n) => {
                self.buf.truncate(len + n);
                Ok(())
            }
            Err(e) => {
                self.buf.truncate(len);
                // An interrupted read consumed nothing; the caller loops.
                if e.kind() == io::ErrorKind::Interrupted {
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Locate `code` at a window offset, reported against the whole stream.
    pub(crate) fn error_at(&self, code: ErrorCode, at: usize) -> StreamError {
        StreamError::Parse(Error::new(code, self.origin + at))
    }
}
