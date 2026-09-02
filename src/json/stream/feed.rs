//! Incremental reading driven from the outside: chunks in, values out.

use std::marker::PhantomData;

use crate::json::traits::Read;
use crate::options::{Options, Standard};
use crate::stream::{DEFAULT_BUFFER, Split};

use super::StreamResult;
use super::split::{Mode, Splitter};
use super::window::{self, Window};

/// A JSON reader you push bytes into.
///
/// [`Documents`](super::Documents) pulls from a reader, which suits a file. A
/// `Feed` is the same machine with the control inverted, which suits anything
/// that hands you bytes when it feels like it: a socket, an event loop, a
/// decompressor, a callback.
///
/// Chunks may split a value at any byte, including inside a string, inside an
/// escape, or between the digits of a number. The scan that finds where a
/// value ends carries its state across the boundary; nothing is re-examined
/// when the next chunk lands.
///
/// ```
/// # #[derive(Default, Debug, PartialEq)] struct Rec { id: u64, tag: String }
/// # structio::object!(Rec { id, tag });
/// let mut feed = structio::Feed::values();
///
/// // A chunk boundary inside a string, and another inside a number.
/// feed.push(br#"{"id":12"#);
/// assert!(feed.next_value::<Rec>().is_none());
/// feed.push(br#"3,"tag":"a\"b"#);
/// assert!(feed.next_value::<Rec>().is_none());
/// feed.push(br#""}"#);
///
/// let rec = feed.next_value::<Rec>().unwrap().unwrap();
/// assert_eq!(rec, Rec { id: 123, tag: "a\"b".into() });
/// ```
///
/// # Completion
///
/// A value is returned once all of its bytes are present, not before: there is
/// no half-filled struct. See the [module documentation](super) for why, and
/// for what that costs.
///
/// Some values cannot be recognized as finished from their own bytes: a bare
/// top-level scalar, since `42` might still become `421`, and in
/// [`Mode::Lines`] a final record with no trailing newline. Call [`Feed::end`]
/// when the input is finished so those complete.
///
/// `O` is the [read policy](crate::Options) every value is read under. The
/// constructors give you [`Standard`]; [`Feed::with_options`] changes it.
pub struct Feed<O: Options = Standard> {
    win: Window,
    options: PhantomData<fn() -> O>,
}

impl Default for Feed {
    fn default() -> Self {
        Self::values()
    }
}

impl Feed {
    /// Whole JSON values one after another, separated by optional whitespace.
    pub fn values() -> Self {
        Self::new(Mode::Values)
    }

    /// Newline-delimited JSON: one value per line, blank lines ignored.
    pub fn lines() -> Self {
        Self::new(Mode::Lines)
    }

    /// The elements of a single top-level array.
    pub fn array() -> Self {
        Self::new(Mode::Array)
    }

    /// Build with an explicit [`Mode`].
    pub fn new(mode: Mode) -> Self {
        Feed {
            win: Window::with_capacity(Splitter::new(mode), DEFAULT_BUFFER),
            options: PhantomData,
        }
    }
}

impl<O: Options> Feed<O> {
    /// Read every value under the policy `P` instead.
    #[must_use = "with_options returns a configured feed and consumes the old one"]
    pub fn with_options<P: Options>(mut self) -> Feed<P> {
        // The splitter divides the stream before the parser sees any of it, so
        // it has to know about comments too: one may hold a brace.
        self.win.framer_mut().set_comments(P::ALLOW_COMMENTS);
        Feed {
            win: self.win,
            options: PhantomData,
        }
    }

    /// Fail rather than buffer more than `bytes` for a single value.
    ///
    /// Unlimited by default. Set this when the bytes come from somewhere you
    /// do not control. It bounds what the feed *retains*, not the size of a
    /// chunk handed to [`Feed::push`]: a single enormous push is resident
    /// because the caller already allocated it.
    #[must_use = "max_value returns a configured feed and consumes the old one"]
    pub fn max_value(mut self, bytes: usize) -> Self {
        self.win.set_limit(bytes);
        self
    }

    /// Add bytes to the stream.
    ///
    /// Cheap; the scanning happens in [`Feed::next_value`]. Bytes pushed after
    /// framing has failed are discarded, so pushing at a dead feed cannot
    /// grow it.
    pub fn push(&mut self, bytes: &[u8]) {
        self.win.extend(bytes);
    }

    /// Declare the input finished.
    ///
    /// Completes a trailing top-level scalar, and turns a value left half
    /// written into an error rather than an indefinite wait. Pushing after
    /// this has no effect on values already resolved, and the remaining bytes
    /// are still parsed under the assumption that no more will follow.
    ///
    /// After this, [`Feed::next_value`] returning `None` means the stream is
    /// finished: nothing more can arrive, so there is nothing else it could
    /// be waiting for. Pushing again after a clean end resumes reading;
    /// pushing after a framing failure does nothing.
    pub fn end(&mut self) {
        self.win.set_eof();
    }

    /// Bytes held but not yet resolved into a value.
    pub fn buffered(&self) -> usize {
        self.win.buffered()
    }

    /// Byte offset in the stream of the next value to be produced.
    pub fn offset(&self) -> usize {
        self.win.offset()
    }

    /// The next complete value, which may borrow from the internal buffer.
    ///
    /// `None` means "not yet": either more bytes are needed, or, after
    /// [`Feed::end`], the stream finished cleanly.
    ///
    /// A value that fails to *parse* is reported and skipped; the framing is
    /// still intact, so the next value is read normally. That is what makes
    /// per-record error recovery work for a file of records.
    ///
    /// A failure to *frame* is different: the position in the input is no
    /// longer known, so there is nothing honest to resume from. It is reported
    /// once and ends the feed, rather than being returned forever and
    /// turning the natural `while let` loop into a spin.
    pub fn next_value<'a, T: Read<'a> + Default>(&'a mut self) -> Option<StreamResult<T>> {
        match self.locate() {
            Ok(Some(span)) => Some(window::parse::<O, T>(&self.win, span)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }

    /// The next complete value, read into one you already have.
    pub fn next_value_into<T: for<'de> Read<'de>>(
        &mut self,
        value: &mut T,
    ) -> Option<StreamResult<()>> {
        match self.locate() {
            Ok(Some(span)) => Some(window::parse_into::<O, T>(&self.win, span, value)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }

    fn locate(&mut self) -> StreamResult<Option<(usize, usize)>> {
        match self.win.try_next()? {
            Split::Item { start, end } => Ok(Some((start, end))),
            // Both mean "nothing to hand back". Which one it is only matters
            // after `end`, and `is_done` is how that is asked.
            Split::Need | Split::End => Ok(None),
        }
    }
}

impl<O: Options> core::fmt::Debug for Feed<O> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Feed")
            .field("buffered", &self.buffered())
            .field("offset", &self.offset())
            .finish_non_exhaustive()
    }
}
