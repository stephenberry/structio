//! Pulling a sequence of JSON values out of an [`io::Read`].

use std::io;
use std::marker::PhantomData;

use crate::json::traits::Read;
use crate::options::{Options, Standard};

use crate::stream::{DEFAULT_BUFFER, Split};

use super::split::{Mode, Splitter};
use super::window::{self, Window};
use super::{StreamError, StreamResult};

/// A reader turned into a series of JSON values.
///
/// One value is held at a time. The buffer keeps a working window over the
/// stream, compacted as values are consumed, so a file of a million records
/// costs roughly one record plus one read, not a million records.
///
/// Pick the constructor that matches how the producer laid the values out:
/// [`Documents::lines`] for newline-delimited records, [`Documents::array`]
/// for the elements of one big array, [`Documents::values`] for bare values
/// back to back.
///
/// `O` is the [read policy](crate::Options) every value is read under. The
/// constructors give you [`Standard`]; [`Documents::with_options`] changes it,
/// as one more link in the same builder chain that sets the size limits.
pub struct Documents<R, O: Options = Standard> {
    reader: R,
    win: Window,
    chunk: usize,
    options: PhantomData<fn() -> O>,
}

impl<R: io::Read> Documents<R> {
    /// Newline-delimited JSON: one value per line, blank lines ignored.
    pub fn lines(reader: R) -> Self {
        Self::new(reader, Mode::Lines)
    }

    /// The elements of a single top-level array.
    pub fn array(reader: R) -> Self {
        Self::new(reader, Mode::Array)
    }

    /// Whole JSON values one after another, separated by optional whitespace.
    ///
    /// A single document is the one-value case, but note that it buys nothing
    /// over [`from_reader`](crate::from_reader) there: one value is buffered
    /// whole either way.
    pub fn values(reader: R) -> Self {
        Self::new(reader, Mode::Values)
    }

    /// Build with an explicit [`Mode`].
    pub fn new(reader: R, mode: Mode) -> Self {
        Documents {
            reader,
            win: Window::new(Splitter::new(mode), DEFAULT_BUFFER),
            chunk: DEFAULT_BUFFER,
            options: PhantomData,
        }
    }
}

impl<R: io::Read, O: Options> Documents<R, O> {
    /// Read every value under the policy `P` instead.
    ///
    /// ```
    /// # #[derive(Default, Debug, PartialEq)] struct Rec { id: u64 }
    /// # structio::object!(Rec { id });
    /// use structio::{Documents, SkipUnknown};
    ///
    /// let input = &b"{\"id\":1,\"note\":\"ignored\"}"[..];
    /// let mut docs = Documents::lines(input).with_options::<SkipUnknown>();
    /// assert_eq!(docs.iter::<Rec>().next().unwrap().unwrap(), Rec { id: 1 });
    /// ```
    #[must_use = "with_options returns a configured reader and consumes the old one"]
    pub fn with_options<P: Options>(mut self) -> Documents<R, P> {
        // The splitter divides the stream before the parser sees any of it, so
        // it has to know about comments too: one may hold a brace.
        self.win.framer_mut().set_comments(P::ALLOW_COMMENTS);
        Documents {
            reader: self.reader,
            win: self.win,
            chunk: self.chunk,
            options: PhantomData,
        }
    }

    /// Fail rather than buffer more than `bytes` for a single value.
    ///
    /// Unlimited by default. Set this when the producer is not trusted: it is
    /// what stops a stream that never closes a bracket from consuming all
    /// available memory. Reads are clipped so the window never runs more than
    /// a byte past the limit before the failure is noticed.
    #[must_use = "max_value returns a configured reader and consumes the old one"]
    pub fn max_value(mut self, bytes: usize) -> Self {
        self.win.set_limit(bytes);
        self
    }

    /// How many bytes to request per read. Defaults to 64 KiB.
    #[must_use = "read_size returns a configured reader and consumes the old one"]
    pub fn read_size(mut self, bytes: usize) -> Self {
        self.chunk = bytes.max(1);
        self
    }

    /// Bytes read but not yet resolved into a value.
    pub fn buffered(&self) -> usize {
        self.win.buffered()
    }

    /// Byte offset in the stream of the next value to be read.
    pub fn offset(&self) -> usize {
        self.win.offset()
    }

    /// Recover the underlying reader, discarding the window.
    ///
    /// Reading is done a chunk at a time, so bytes past the last value
    /// returned have usually already been taken from the reader, and those are
    /// lost. This mirrors [`io::BufReader::into_inner`], which is lossy for the
    /// same reason. Use it to finish with a reader, not to hand a live stream
    /// on to something else; [`Documents::into_parts`] is the lossless form.
    ///
    /// [`io::BufReader::into_inner`]: std::io::BufReader::into_inner
    pub fn into_inner(self) -> R {
        self.reader
    }

    /// Recover the underlying reader together with the bytes already taken
    /// from it that did not become a value.
    ///
    /// Concatenating the returned bytes with everything still in the reader
    /// reconstructs the remainder of the stream exactly, which is what makes
    /// it safe to hand a partly consumed stream to something else.
    ///
    /// The bytes are empty when framing has failed, since the position in the
    /// stream is no longer known and there is nothing honest to resume from.
    pub fn into_parts(self) -> (R, Vec<u8>) {
        (self.reader, self.win.into_unread())
    }

    /// The next value, which may borrow from the stream buffer.
    ///
    /// The borrow is of `self`, so the reader cannot advance while the value
    /// is alive. That is what makes zero-copy `&str` fields work here: they
    /// point into the window, and the window is pinned until you drop them.
    /// For values that own their data, [`Documents::iter`] is an ordinary
    /// iterator and reads better in a loop.
    ///
    /// `None` means the stream ended, either cleanly or because framing has
    /// already failed and the failure was reported.
    ///
    /// A value that fails to *parse* is reported and skipped; the framing is
    /// still intact, so the next value is read normally. That is what makes
    /// per-record error recovery work for a file of records.
    ///
    /// A failure to *frame* is different: the position in the input is no
    /// longer known, so there is nothing honest to resume from. It is reported
    /// once and ends the stream, rather than being returned forever and
    /// turning the natural `while let` loop into a spin.
    pub fn next_value<'a, T: Read<'a> + Default>(&'a mut self) -> Option<StreamResult<T>> {
        match self.locate() {
            Ok(Some(span)) => Some(window::parse::<O, T>(&self.win, span)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }

    /// The next value, read into one you already have.
    ///
    /// Mirrors [`read_into`](crate::read_into): `value` keeps its allocations
    /// between calls, so a loop over a million records of the same shape
    /// settles into doing no allocation at all.
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

    /// Iterate over owned values.
    ///
    /// ```
    /// # #[derive(Default, Debug, PartialEq)] struct Rec { id: u64 }
    /// # structio::object!(Rec { id });
    /// let mut docs = structio::Documents::lines(&b"{\"id\":1}\n{\"id\":2}"[..]);
    /// for value in docs.iter::<Rec>() {
    ///     println!("{}", value.unwrap().id);
    /// }
    /// ```
    pub fn iter<T: for<'de> Read<'de> + Default>(&mut self) -> Iter<'_, R, T, O> {
        Iter {
            docs: self,
            marker: PhantomData,
        }
    }

    /// Read until the splitter can name a whole value, or the stream ends.
    fn locate(&mut self) -> StreamResult<Option<(usize, usize)>> {
        loop {
            match self.win.try_next()? {
                Split::Item { start, end } => return Ok(Some((start, end))),
                Split::End => return Ok(None),
                Split::Need => {
                    // Defensive: the splitter resolves every state at end of
                    // input, so `Need` should not come back once `eof` is set.
                    // Were it to, filling again would read zero forever.
                    if self.win.is_eof() {
                        return Err(StreamError::Parse(crate::Error::new(
                            crate::ErrorCode::UnexpectedEnd,
                            self.win.offset(),
                        )));
                    }
                    self.win.fill(&mut self.reader, self.chunk)?;
                }
            }
        }
    }
}

/// Iterator over owned values, from [`Documents::iter`].
pub struct Iter<'d, R, T, O: Options = Standard> {
    docs: &'d mut Documents<R, O>,
    marker: PhantomData<fn() -> T>,
}

impl<R: io::Read, T: for<'de> Read<'de> + Default, O: Options> Iterator for Iter<'_, R, T, O> {
    type Item = StreamResult<T>;

    fn next(&mut self) -> Option<Self::Item> {
        // Terminating after a failure is `Documents`' own rule, so there is
        // nothing extra to do here.
        self.docs.next_value::<T>()
    }
}
