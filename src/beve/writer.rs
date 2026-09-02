//! The BEVE output buffer.
//!
//! The same shape as [`json::Writer`](crate::json::Writer): append into a
//! `Vec<u8>`, reserve once per value, and optionally drain into an
//! [`io::Write`] as the buffer fills so the document never exists in memory
//! all at once.
//!
//! It is a separate type rather than a mode of the JSON writer because the two
//! hold different invariants. The JSON buffer is always valid UTF-8 and always
//! has a rewritable trailing comma, which is why its drain keeps a byte back
//! and cuts on a character boundary. Neither applies to binary, so this drain
//! hands over everything it has.

use std::io;
use std::marker::PhantomData;

use crate::beve::header::{self, encode_size};
use crate::beve::impls::{Block, NumericBytes};
use crate::beve::traits::{Write, WriteArray, WriteAs, WriteKeyAs, WriteObject};
use crate::options::{Options, Standard};

/// How much a sink-backed writer buffers before draining.
pub const DEFAULT_SINK_BUFFER: usize = 8 * 1024;

/// The width an array under `header` pads its payload to, or `None` to write
/// the plain form.
///
/// The aligned form is defined for numeric arrays alone, and asking for an
/// element width is what tells them apart: booleans and strings share the
/// typed-array type with the numbers but are stored under a category that has
/// no width, so they decline here alongside the width codes that name no
/// element at all.
#[inline]
const fn padded_width(header: u8) -> Option<usize> {
    if header::ty(header) != header::TY_TYPED_ARRAY {
        // Not an array at all. The plain path would write the caller's mistake
        // through unchanged; wrapping it in the marker as well would turn a
        // header no reader wanted into bytes no reader can even step over.
        return None;
    }
    match header::byte_width(header::sub(header), header::count(header)) {
        // One-byte elements are aligned wherever they land, so padding them
        // would spend two bytes and a less widely implemented form on nothing.
        Some(width) if width > 1 => Some(width),
        _ => None,
    }
}

/// Accumulates BEVE output.
///
/// The lifetime is the borrow of an [`io::Write`] sink, and is `'static` for
/// the ordinary in-memory writers built by [`Writer::new`] and friends.
///
/// `O` is the [write policy](crate::Options). Only [`Options::SKIP_NULL`]
/// means anything to a binary format; indentation has nowhere to go. It
/// defaults to [`Standard`], though a constructor cannot infer from that, so
/// build one as `Writer::<Standard>::new()`. Trait implementations take
/// `&mut Writer<'_, O>` and stay generic over it.
pub struct Writer<'a, O: Options = Standard> {
    buf: Vec<u8>,
    /// Highest length an append may reach on the fast path.
    ///
    /// Without a sink this is exactly `buf.capacity()`, so testing against it
    /// is the capacity test `Vec` would have made anyway. With one it is the
    /// lesser of the capacity and the drain threshold, which folds "is there
    /// room" and "is it time to drain" into that single compare.
    limit: usize,
    /// Buffer size to drain back down to, or `usize::MAX` with no sink, which
    /// makes `limit` collapse to the capacity and every drain a no-op.
    threshold: usize,
    /// Bytes of this document that stand in front of the buffer. Two things put
    /// bytes here: a drain, which moves them out of the buffer, and
    /// [`Self::at`], which says some other code wrote them. Zero for a writer
    /// with no sink that begins the document, where the buffer is the whole of
    /// it.
    ///
    /// Under [`Options::MEASURE`] nothing ever enters the buffer, so this
    /// alone is the extent reached, and [`Self::offset`] still answers
    /// correctly for the padding that depends on it.
    origin: usize,
    /// Bytes at the front of the buffer that belong to no document of this
    /// writer's, so that `origin + buf.len() - skip` is the offset of the next
    /// byte from the start of the document.
    ///
    /// Only [`Self::at`] sets it, and only where the document begins *inside*
    /// what the buffer already holds: a frame appended to a send buffer that
    /// still holds the frames before it. Those earlier bytes are carried along
    /// and handed back, and they are not part of this document, so the padding
    /// cannot be measured against them.
    ///
    /// It never changes afterwards. A drain moves the whole buffer into
    /// `origin`, which leaves `origin + buf.len()` larger by exactly what left,
    /// so one correction subtracted once stays correct for the life of the
    /// writer.
    skip: usize,
    /// Whether numeric typed arrays take the aligned form. See
    /// [`Writer::aligned`].
    aligned: bool,
    /// Members written into the object currently being built, checked against
    /// what [`WriteObject::count_fields`] promised. Debug builds only: it
    /// guards against a silent corruption rather than against anything a
    /// release build could act on.
    #[cfg(debug_assertions)]
    members: usize,
    sink: Option<Sink<'a>>,
    /// `fn() -> O` rather than `O`, so the writer's auto traits depend on what
    /// it holds rather than on a policy type it never contains.
    options: PhantomData<fn() -> O>,
}

struct Sink<'a> {
    out: &'a mut dyn io::Write,
    /// The first write failure. [`Write`] is infallible by design, so an error
    /// is recorded here and surfaced once, by [`Writer::finish`]. Everything
    /// after it is discarded rather than buffered, which keeps a failed writer
    /// from growing without bound.
    err: Option<io::Error>,
    /// [`Writer::finish`] ran. Only read by the drop check.
    finished: bool,
}

impl Drop for Sink<'_> {
    /// A sink writer dropped without [`Writer::finish`] silently truncates its
    /// output and reports no error for it. Nothing in the type system prevents
    /// that, so it is at least loud in a debug build.
    fn drop(&mut self) {
        debug_assert!(
            self.finished,
            "structio: a sink Writer was dropped without `finish`, truncating its output"
        );
    }
}

impl<O: Options> Default for Writer<'static, O> {
    fn default() -> Self {
        Self::new()
    }
}

impl<O: Options> Writer<'static, O> {
    #[inline]
    pub fn new() -> Self {
        Writer::appending(Vec::new())
    }

    #[inline]
    pub fn with_capacity(n: usize) -> Self {
        Writer::appending(Vec::with_capacity(n))
    }

    /// Reuse an existing buffer, discarding whatever it held.
    ///
    /// [`Self::appending`] is the one that keeps it.
    #[inline]
    pub fn from_vec(mut buf: Vec<u8>) -> Self {
        buf.clear();
        Writer::appending(buf)
    }

    /// Write after what a buffer already holds, rather than over it.
    ///
    /// [`Self::from_vec`] discards the contents; this keeps them, and counts
    /// them as part of the document. That is what a value written behind a
    /// header needs: padding is measured from where the document began, so a
    /// value appended behind a prefix pads against where its bytes will really
    /// land rather than against its own first byte. [`Self::at`] is for the
    /// case where the buffer is not the start of the document either.
    #[inline]
    pub fn appending(buf: Vec<u8>) -> Self {
        Writer {
            limit: buf.capacity(),
            threshold: usize::MAX,
            origin: 0,
            skip: 0,
            aligned: false,
            buf,
            sink: None,
            options: PhantomData,
            #[cfg(debug_assertions)]
            members: 0,
        }
    }
}

/// Write into `out` in place, giving the buffer back whatever happens.
///
/// The buffer has to move into the writer, so an unwind out of `write` would
/// drop it, and with it whatever the caller had put in front of this document:
/// a protocol header, or the frames already queued behind it. Those bytes are
/// the one part of the buffer the call was never meant to touch, and an unwind
/// is in contract rather than a caller's bug, since a
/// [`WriteAs`](crate::beve::WriteAs) adapter whose target has values it cannot
/// encode is told to write a substitute or panic.
///
/// So `out` comes back holding the whole buffer if `write` returned, and
/// exactly the bytes it held before if `write` unwound.
pub(crate) fn append_in_place<O: Options>(
    out: &mut Vec<u8>,
    make: impl FnOnce(Vec<u8>) -> Writer<'static, O>,
    write: impl FnOnce(&mut Writer<'static, O>),
) {
    /// Holds the buffer for the write and hands it back in `Drop`, which is
    /// what makes the unwind path and the ordinary one the same code.
    struct Handback<'o, O: Options> {
        w: Writer<'static, O>,
        out: &'o mut Vec<u8>,
        /// How much of the buffer to keep: the length the caller's own bytes
        /// had, until the value is written whole and there is nothing to cut.
        keep: usize,
    }

    impl<O: Options> Drop for Handback<'_, O> {
        fn drop(&mut self) {
            // By hand rather than through `into_vec`, which takes the writer
            // by value; a `Drop` has only the borrow.
            let mut buf = core::mem::take(&mut self.w.buf);
            buf.truncate(self.keep);
            *self.out = buf;
        }
    }

    let keep = out.len();
    let mut h = Handback {
        w: make(core::mem::take(out)),
        out,
        keep,
    };
    write(&mut h.w);
    h.keep = usize::MAX;
}

impl<'a, O: Options> Writer<'a, O> {
    /// Write through to `out`, buffering [`DEFAULT_SINK_BUFFER`] bytes at a
    /// time.
    ///
    /// The buffer never grows past that size, however large the document or
    /// any single value in it: a block that would not fit is handed to the
    /// sink directly rather than copied in first.
    ///
    /// [`Writer::finish`] must be called to flush the tail and report any I/O
    /// error; [`beve::to_writer`](crate::beve::to_writer) does both.
    #[inline]
    pub fn to_sink(out: &'a mut dyn io::Write) -> Self {
        Self::to_sink_with_capacity(out, DEFAULT_SINK_BUFFER)
    }

    /// [`Writer::to_sink`] with an explicit buffer size.
    pub fn to_sink_with_capacity(out: &'a mut dyn io::Write, capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let buf = Vec::with_capacity(capacity);
        Writer {
            limit: buf.capacity().min(capacity),
            threshold: capacity,
            origin: 0,
            skip: 0,
            aligned: false,
            #[cfg(debug_assertions)]
            members: 0,
            buf,
            sink: Some(Sink {
                out,
                err: None,
                finished: false,
            }),
            options: PhantomData,
        }
    }

    /// Store numeric typed arrays in the aligned form, for a reader that means
    /// to point at them rather than copy them.
    ///
    /// A typed array's payload begins wherever its header and count leave off,
    /// which is almost never a multiple of the element width, so a reader that
    /// wants a `&[f64]` out of the document has to copy the block somewhere
    /// aligned first. The aligned form states the element type in a second
    /// header and inserts a run of padding chosen so the payload starts at a
    /// multiple of its element width, counted from the start of the document.
    /// A reader whose own buffer is aligned can then borrow the block in
    /// place.
    ///
    /// Only numeric arrays change, and only those wider than one byte:
    /// booleans and strings have no aligned form, and one-byte elements are
    /// aligned wherever they land. A document with no numeric array wider than
    /// a byte in it comes out byte for byte the same. Both forms are ordinary
    /// BEVE and this crate reads either in one copy, so what this costs is the
    /// padding and a form that a decoder is less likely to have implemented.
    ///
    /// A hand-written [`Write`] impl takes part only if it opens its arrays
    /// with [`Self::begin_typed_array`]; one that spells the preamble out with
    /// [`Self::push`] and [`Self::size`] keeps the plain form whatever this
    /// says.
    ///
    /// The offsets are counted from the start of the document, which by
    /// default is where this writer's output begins. A value that will sit
    /// behind a prefix pads against that prefix instead, which is what
    /// [`Self::appending`] and [`Self::at`] are for. Told nothing, a writer
    /// counts from its own zero, and the payloads land on their element width
    /// only if the prefix is a multiple of 16 bytes, 16 being the widest
    /// element BEVE has; a narrower multiple is enough for a document whose own
    /// widest array is narrower.
    #[inline]
    pub fn aligned(mut self) -> Self {
        self.aligned = true;
        self
    }

    /// State where in the document this writer stands: the next byte it writes
    /// is the one at `offset`.
    ///
    /// Everything positional is then measured from the document's start rather
    /// than from this writer's own. [`Self::offset`] answers `offset` straight
    /// afterwards, and the aligned form pads each numeric payload onto its
    /// element width *there*, which is the only place the padding is worth
    /// anything. Told nothing, a writer takes its first byte for the document's
    /// first byte, since that is all it can see.
    ///
    /// The bytes in front are not written, held, or produced here. They are the
    /// ones some other code has written, or will write: a frame header already
    /// sent to a socket, or a prefix in the buffer this one is appending to.
    /// Where the buffer holds them already, [`Self::appending`] has counted
    /// them, and this says the same thing over again rather than something new:
    /// `appending(frame).at(frame.len())` is `appending(frame)`.
    ///
    /// An offset *inside* what the buffer holds is the interesting case rather
    /// than a mistake. A send buffer accumulating frames back to back still
    /// holds the ones before this one, and they belong to a different document,
    /// so what this writer stands at is its position in *its own* frame:
    /// `appending(send).at(send.len() - frame_start)`. Leaving it out would pad
    /// against the whole send buffer, aligning the payload for a reader that
    /// receives the buffer entire rather than for one that receives a frame.
    /// Those earlier bytes are carried along and handed back untouched either
    /// way.
    ///
    /// ```
    /// use structio::Standard;
    /// use structio::beve::Writer;
    ///
    /// let w = Writer::<Standard>::new().aligned().at(48);
    /// assert_eq!(w.offset(), 48);
    /// ```
    #[inline]
    pub fn at(mut self, offset: usize) -> Self {
        debug_assert!(
            offset <= isize::MAX as usize,
            "structio: `at` was given an offset no document can reach"
        );
        // Two fields for one number, because the document may begin either in
        // front of the buffer or inside it, and neither a `usize` nor the
        // wrapping arithmetic that would fake one can hold both directions.
        match offset.checked_sub(self.buf.len()) {
            Some(before) => {
                self.origin = before;
                self.skip = 0;
            }
            None => {
                self.origin = 0;
                self.skip = self.buf.len() - offset;
            }
        }
        debug_assert_eq!(self.offset(), offset);
        self
    }

    /// The offset of the next byte from the start of the document.
    ///
    /// **This, and not [`len`](Self::len), is where you are.** The two agree
    /// only for a writer that is assembling the whole document in memory. With
    /// a sink the buffer holds the tail alone, so the bytes already drained
    /// have to be counted; while measuring, nothing enters the buffer at all
    /// and this is the entire extent so far. Padding is measured from where
    /// the document began, not from where the buffer does, which is what makes
    /// this the one figure an implementation that lays out its own value can
    /// depend on.
    #[inline]
    pub fn offset(&self) -> usize {
        debug_assert!(self.origin + self.buf.len() >= self.skip);
        self.origin + self.buf.len() - self.skip
    }

    /// The bytes written so far, or with a sink, the bytes not yet drained.
    ///
    /// Buffer contents, not the document: see [`offset`](Self::offset).
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        self.not_measuring();
        &self.buf
    }

    /// Take the buffer.
    ///
    /// With a sink this is only the undrained tail, which is not the document;
    /// use [`Writer::finish`] there instead.
    #[inline]
    pub fn into_vec(self) -> Vec<u8> {
        self.not_measuring();
        self.buf
    }

    /// Bytes currently buffered.
    ///
    /// Not the length of the document and not a position within it. A sink
    /// writer empties this on every drain, and a measuring one never fills it,
    /// so an implementation that wants to know where it is wants
    /// [`offset`](Self::offset) instead.
    #[inline]
    pub fn len(&self) -> usize {
        self.not_measuring();
        self.buf.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.not_measuring();
        self.buf.is_empty()
    }

    /// Refuse, in a debug build, to answer a question about the buffer while
    /// measuring, where the honest answer is always "empty".
    ///
    /// The four accessors above report buffer contents, which under
    /// [`Options::MEASURE`] describe nothing. A value laid out by hand from
    /// what [`len`](Self::len) returned would measure differently from the way
    /// it writes, silently, and the wrong number would go into a frame header.
    /// So the mistake is made loud instead, in the same spirit as the
    /// member-count check in [`Self::write_object`].
    #[inline]
    fn not_measuring(&self) {
        debug_assert!(
            !O::MEASURE,
            "structio: a measuring writer was asked about its buffer, which is always empty; \
             use `Writer::offset` for the position in the document"
        );
    }

    /// Drain the remaining bytes to the sink and report the first I/O error.
    ///
    /// Without a sink this is `Ok(())` and does nothing.
    ///
    /// By value on purpose: a sink writer that kept going after its tail had
    /// been flushed would emit a second document's bytes into the middle of
    /// the first.
    pub fn finish(mut self) -> io::Result<()> {
        self.not_measuring();
        let Some(sink) = self.sink.as_mut() else {
            return Ok(());
        };
        sink.finished = true;
        if sink.err.is_none()
            && !self.buf.is_empty()
            && let Err(e) = sink.out.write_all(&self.buf)
        {
            sink.err = Some(e);
        }
        self.buf.clear();
        match sink.err.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    // -----------------------------------------------------------------------
    // Buffer management
    // -----------------------------------------------------------------------

    /// Account for `n` bytes without storing them, and report whether that was
    /// the whole of the append.
    ///
    /// The one thing measuring means, in the one place that says so. Every
    /// append below opens with this and returns when it answers `true`, so a
    /// measurement is the writer's own arithmetic about its own output rather
    /// than a second description of the format that could disagree with it.
    /// The count it keeps is [`Self::offset`], which is what the aligned form
    /// pads against, so alignment is measured exactly rather than estimated.
    ///
    /// [`Options::MEASURE`] is a constant, so one arm of this survives per
    /// instantiation: an ordinary writer never tests it and a measuring one
    /// never reaches the stores past it.
    #[inline(always)]
    fn measuring(&mut self, n: usize) -> bool {
        if O::MEASURE {
            self.origin += n;
            true
        } else {
            false
        }
    }

    /// The offset measuring reached.
    ///
    /// Only meaningful under [`Options::MEASURE`], where nothing enters the
    /// buffer and the offset of the next byte is everything counted so far.
    /// That is the length of the value alone for a writer that began the
    /// document, and the length plus the offset it was given for one that did
    /// not, so a caller that supplied an offset subtracts it back off.
    #[inline]
    pub(crate) fn measured(&self) -> usize {
        debug_assert!(
            O::MEASURE,
            "structio: `measured` on a writer that is producing bytes rather than counting them"
        );
        // The completeness check on the guarded set, and it is a total one
        // rather than a heuristic. A measuring writer is built by
        // `Writer::new`, so it has no sink, and both `drain` and `hand_over`
        // return early without one: nothing can empty the buffer again. So if
        // any append stored a byte, it is still here, and an append added
        // later without its `measuring` guard fails at every `beve::size` call
        // in the suite rather than only where some test happened to reach it.
        debug_assert!(
            self.buf.is_empty(),
            "structio: a measuring writer stored {} bytes, so some append is missing its \
             `measuring` guard",
            self.buf.len()
        );
        self.offset()
    }

    /// Guarantee room for `n` more bytes, draining or growing if there is not.
    ///
    /// Every append goes through here, and on the fast path it is the single
    /// compare `Vec` would have made on its own, which is what makes draining
    /// cost the in-memory path nothing.
    #[inline(always)]
    fn room(&mut self, n: usize) {
        if self.buf.len() + n > self.limit {
            self.spill(n);
        }
    }

    /// The out-of-line half of [`Self::room`]: make room, then restate the
    /// limit.
    #[cold]
    fn spill(&mut self, n: usize) {
        self.drain();
        if self.buf.len() + n > self.buf.capacity() {
            self.buf.reserve(n);
        }
        // The only assignment to `limit` outside construction. The `min` keeps
        // `limit <= capacity`, which every `set_len` below depends on.
        self.limit = self.buf.capacity().min(self.threshold);
    }

    /// Write a block straight to the sink, bypassing the buffer, and report
    /// whether it was taken.
    ///
    /// Reached only from [`Self::raw`]'s cold path, and only for a block at
    /// least as large as the whole buffer. Anything smaller is worth batching,
    /// and buffering it costs nothing that was not already paid for; anything
    /// larger the buffer could only grow to hold and then hand over unchanged.
    /// Declining leaves the caller to make room as usual, so this is an
    /// optimization and never the only path.
    ///
    /// With no sink `threshold` is `usize::MAX` and no block can reach it, so
    /// an in-memory writer never takes this path and keeps every byte.
    #[cold]
    fn hand_over(&mut self, bytes: &[u8]) -> bool {
        if bytes.len() < self.threshold {
            return false;
        }
        // Whatever is already buffered was written first and has to leave
        // first, or the document comes out reordered.
        self.drain();
        let Some(sink) = self.sink.as_mut() else {
            return false;
        };
        if sink.err.is_none()
            && let Err(e) = sink.out.write_all(bytes)
        {
            sink.err = Some(e);
        }
        // The block never entered the buffer, but it is still part of the
        // document, so the offset that padding is measured against has to
        // account for it.
        self.origin += bytes.len();
        // Taken either way: a recorded error means the rest of the document is
        // discarded, not buffered.
        true
    }

    /// Hand the whole buffer to the sink.
    ///
    /// Nothing is held back. A BEVE value is written front to back and never
    /// revisited: sizes are known before their payloads, so no byte already
    /// emitted is ever rewritten.
    fn drain(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let Some(sink) = self.sink.as_mut() else {
            return;
        };
        if sink.err.is_none()
            && let Err(e) = sink.out.write_all(&self.buf)
        {
            sink.err = Some(e);
        }
        // Discard even on failure, so a broken sink does not turn into an
        // unbounded allocation. Dropped bytes still happened as far as the
        // document is concerned: they were counted, and what follows is padded
        // against where it will land in the output the sink received.
        self.origin += self.buf.len();
        self.buf.clear();
    }

    /// Append one byte.
    #[inline(always)]
    pub fn push(&mut self, b: u8) {
        if self.measuring(1) {
            return;
        }
        self.room(1);
        let len = self.buf.len();
        // SAFETY: `room(1)` leaves `capacity >= len + 1`, so the byte lands
        // inside the allocation and the new length is within capacity.
        unsafe {
            self.buf.as_mut_ptr().add(len).write(b);
            self.buf.set_len(len + 1);
        }
    }

    /// Append bytes verbatim.
    ///
    /// A block at least as large as a sink writer's whole buffer goes straight
    /// to the sink instead of through it. Copying it in would mean growing the
    /// buffer to hold a run that is about to be handed over unchanged, which
    /// is how a long string or a large typed array came to be buffered whole.
    #[inline(always)]
    pub fn raw(&mut self, bytes: &[u8]) {
        if self.measuring(bytes.len()) {
            return;
        }
        // The same single compare `room` makes, with both of its cold
        // halves behind it: hand the block over, or make room for it as usual.
        if self.buf.len() + bytes.len() > self.limit {
            if self.hand_over(bytes) {
                return;
            }
            self.spill(bytes.len());
        }
        let len = self.buf.len();
        // SAFETY: either the compare above passed, and `limit <= capacity`
        // gives `capacity >= len + bytes.len()`, or `spill` ran and reserved
        // for exactly that. `bytes` is a distinct allocation from the buffer.
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.buf.as_mut_ptr().add(len),
                bytes.len(),
            );
            self.buf.set_len(len + bytes.len());
        }
    }

    // -----------------------------------------------------------------------
    // Primitives
    // -----------------------------------------------------------------------

    /// Append a number at its natural width, with its type header: a header
    /// byte followed by `N` bytes of little-endian payload, in one reserve.
    ///
    /// `tag` and `N` are both constants at every call site, so this lowers to
    /// one compare and a couple of stores rather than a `memcpy` call.
    #[inline(always)]
    pub fn write_number<const N: usize>(&mut self, tag: u8, payload: [u8; N]) {
        if self.measuring(N + 1) {
            return;
        }
        self.room(N + 1);
        let len = self.buf.len();
        // SAFETY: `room(N + 1)` guarantees `capacity >= len + N + 1`, so both
        // stores stay inside the allocation.
        unsafe {
            let p = self.buf.as_mut_ptr().add(len);
            p.write(tag);
            core::ptr::copy_nonoverlapping(payload.as_ptr(), p.add(1), N);
            self.buf.set_len(len + N + 1);
        }
    }

    /// Append a lone complex number: the extension header, the class header
    /// that says what a component is, and the two components little end first.
    ///
    /// `class` and `N` are both constants at every call site, exactly as in
    /// [`Self::write_number`], so this lowers to one compare and a handful of
    /// stores.
    #[inline(always)]
    pub fn write_complex<const N: usize>(&mut self, class: u8, re: [u8; N], im: [u8; N]) {
        if self.measuring(2 * N + 2) {
            return;
        }
        self.room(2 * N + 2);
        let len = self.buf.len();
        // SAFETY: `room(2 * N + 2)` guarantees `capacity >= len + 2 * N + 2`,
        // so every store below stays inside the allocation.
        unsafe {
            let p = self.buf.as_mut_ptr().add(len);
            p.write(header::COMPLEX);
            p.add(1).write(class);
            core::ptr::copy_nonoverlapping(re.as_ptr(), p.add(2), N);
            core::ptr::copy_nonoverlapping(im.as_ptr(), p.add(2 + N), N);
            self.buf.set_len(len + 2 * N + 2);
        }
    }

    /// Open a matrix: the extension header and its layout byte.
    ///
    /// The caller writes the extents and then the data, each an ordinary value,
    /// which is what lets a matrix hold whatever a sequence can.
    #[inline]
    pub fn begin_matrix(&mut self, layout: u8) {
        self.push(header::MATRIX);
        self.push(layout);
    }

    /// Append a compressed size.
    #[inline(always)]
    pub fn size(&mut self, n: u64) {
        let mut out = [0u8; 8];
        let used = encode_size(n, &mut out);
        // Still through `encode_size` rather than through `header::size_len`
        // directly, so a measurement takes the width from the code that
        // decides it and inherits its range check besides. The buffer it
        // filled is dead on this path and the optimizer drops it.
        if self.measuring(used) {
            return;
        }
        self.room(8);
        let len = self.buf.len();
        // SAFETY: `room(8)` covers the widest encoding, so writing all eight
        // bytes stays inside the allocation; `used <= 8` keeps the length in
        // capacity, and the bytes past `used` are left in spare capacity.
        // Always copying eight makes the length a constant to `memcpy`.
        unsafe {
            core::ptr::copy_nonoverlapping(out.as_ptr(), self.buf.as_mut_ptr().add(len), 8);
            self.buf.set_len(len + used);
        }
    }

    #[inline(always)]
    pub fn write_null(&mut self) {
        self.push(header::NULL);
    }

    #[inline(always)]
    pub fn write_bool(&mut self, v: bool) {
        self.push(if v { header::TRUE } else { header::FALSE });
    }

    /// Write a string as a standalone value: header, size, bytes.
    #[inline]
    pub fn write_str(&mut self, s: &str) {
        self.push(header::STRING);
        self.write_str_body(s);
    }

    /// Write a string with no header, the form BEVE uses for object keys and
    /// for the elements of a string array.
    #[inline]
    pub fn write_str_body(&mut self, s: &str) {
        self.size(s.len() as u64);
        self.raw(s.as_bytes());
    }

    // -----------------------------------------------------------------------
    // Structural
    // -----------------------------------------------------------------------

    /// Write a struct as a BEVE object.
    ///
    /// The member count comes from
    /// [`WriteObject::count_fields`], which is `KEYS.len()` and folds to a
    /// literal unless [`Options::SKIP_NULL`] can drop a member. So the ordinary
    /// case still writes a constant-size prefix with nothing counted or
    /// patched afterwards.
    ///
    /// A count that disagrees with the members written would corrupt the
    /// document silently, so a debug build checks it.
    #[inline]
    pub fn write_object<T: WriteObject>(&mut self, value: &T) {
        let declared = value.count_fields::<O>();
        self.push(header::OBJECT);
        self.size(declared as u64);
        #[cfg(debug_assertions)]
        let outer = core::mem::replace(&mut self.members, 0);
        value.write_fields(self);
        #[cfg(debug_assertions)]
        {
            assert_eq!(
                self.members,
                declared,
                "structio: `{}` wrote {} members after declaring {}, which would \
                 corrupt the document; `count_fields` must agree with `write_fields`",
                core::any::type_name::<T>(),
                self.members,
                declared,
            );
            self.members = outer;
        }
    }

    /// Write one `SIZE | KEY | VALUE` member. `key` is the pre-encoded key,
    /// assembled at compile time by the macro.
    ///
    /// Under [`Options::SKIP_NULL`] a member holding nothing is not written at
    /// all. The object header has already stated a count
    /// that accounts for it, by way of [`WriteObject::count_fields`].
    #[inline(always)]
    pub fn member<T: Write + ?Sized>(&mut self, key: &[u8], value: &T) {
        if O::SKIP_NULL && value.is_null() {
            return;
        }
        #[cfg(debug_assertions)]
        {
            self.members += 1;
        }
        self.raw(key);
        value.write(self);
    }

    /// Write one `SIZE | KEY | VALUE` member, the value through an adapter.
    ///
    /// [`Self::member`] for a field whose declaration named an adapter. It
    /// counts the member for the debug check exactly as `member` does, and
    /// applies [`Options::SKIP_NULL`] through [`WriteAs::is_null`], which is
    /// the same answer the generated [`WriteObject::count_fields`] subtracted
    /// from the header's count.
    ///
    /// `A` appears in no argument, so it is always turned up explicitly:
    /// `w.member_with::<Millis, _>(key, value)`.
    #[inline(always)]
    pub fn member_with<A: WriteAs<T>, T: ?Sized>(&mut self, key: &[u8], value: &T) {
        if O::SKIP_NULL && A::is_null(value) {
            return;
        }
        #[cfg(debug_assertions)]
        {
            self.members += 1;
        }
        self.raw(key);
        A::write(value, self);
    }

    /// Write an enum variant that carries a value: an object of one member,
    /// keyed by the variant name.
    ///
    /// `key` is the pre-encoded `SIZE | KEY`, assembled at compile time by the
    /// macro, exactly as [`Self::member`] takes one. A variant carrying
    /// nothing is not written here at all: it is its own name, so it goes
    /// through [`Self::write_str`].
    ///
    /// The header and count are written directly rather than through
    /// [`Self::write_object`], which needs a [`WriteObject`] and so a key
    /// schema this type does not have. The count is one by construction, and
    /// [`Options::SKIP_NULL`] deliberately does not reach here: dropping the
    /// member would leave an empty object, which names no variant.
    #[inline]
    pub fn write_tagged<T: Write + ?Sized>(&mut self, key: &[u8], value: &T) {
        self.push(header::OBJECT);
        self.size(1);
        self.raw(key);
        value.write(self);
    }

    /// Write an internally tagged variant carrying a value: an object whose
    /// first member is the tag and whose rest are the payload's own.
    ///
    /// `key` is the pre-encoded `SIZE | KEY` of the tag, and `name` the
    /// variant's name. The member count is the payload's plus one for the tag,
    /// so [`Options::SKIP_NULL`] is accounted for exactly as
    /// [`Self::write_object`] accounts for it: through
    /// [`WriteObject::count_fields`], whose answer has to match what
    /// [`WriteObject::write_fields`] then writes.
    ///
    /// The tag goes first because that is where the reader requires it. BEVE
    /// counts its members rather than closing them with a brace, so a reader
    /// could in principle take them in any order; requiring the tag first
    /// anyway is what keeps one declaration meaning one thing in both formats.
    #[inline]
    pub fn write_internally_tagged<T: WriteObject + ?Sized>(
        &mut self,
        key: &[u8],
        name: &str,
        value: &T,
    ) {
        let declared = value.count_fields::<O>() + 1;
        self.push(header::OBJECT);
        self.size(declared as u64);
        // The tag is a member and is counted as one, so the payload's own
        // members are counted on top of it rather than from zero.
        #[cfg(debug_assertions)]
        let outer = core::mem::replace(&mut self.members, 1);
        self.raw(key);
        name.write(self);
        value.write_fields(self);
        #[cfg(debug_assertions)]
        {
            assert_eq!(
                self.members,
                declared,
                "structio: `{}` wrote {} members after declaring {}, which would \
                 corrupt the document; `count_fields` must agree with `write_fields`",
                core::any::type_name::<T>(),
                self.members,
                declared,
            );
            self.members = outer;
        }
    }

    /// Write an internally tagged variant that carries nothing: an object
    /// holding the tag and no more.
    ///
    /// Unlike [`Self::write_tagged`]'s bare-name form there is no shorter
    /// spelling to fall back on: an internally tagged value is an object
    /// whether or not the variant has anything in it.
    #[inline]
    pub fn write_internally_tagged_unit(&mut self, key: &[u8], name: &str) {
        self.push(header::OBJECT);
        self.size(1);
        self.raw(key);
        name.write(self);
    }

    /// Write a struct as a BEVE array.
    ///
    /// The positional counterpart of [`Self::write_object`]. The element count
    /// is [`Elements::LEN`](crate::Elements::LEN), a compile-time constant, so
    /// the size prefix folds to a literal exactly as an object's member count
    /// does.
    ///
    /// Generic unless the struct was declared with an element type, which is
    /// what says its fields are all one type and so have a typed array to be
    /// stored in. [`WriteArray::ARRAY`] is a constant, so exactly one arm of
    /// this survives per struct.
    #[inline]
    pub fn write_array<T: WriteArray>(&mut self, value: &T) {
        match T::ARRAY {
            None => {
                self.begin_generic_array(T::LEN);
                value.write_elements(self);
            }
            Some(h) => {
                self.begin_array(h, T::LEN);
                value.write_payload(self);
            }
        }
    }

    /// Write one element of an array: the value, and nothing else.
    ///
    /// The array header already gave the count, so unlike the JSON side there
    /// is no separator to carry.
    #[inline(always)]
    pub fn element<T: Write + ?Sized>(&mut self, value: &T) {
        value.write(self);
    }

    /// Write a contiguous sequence, as a typed array when the element type has
    /// one and as a generic array otherwise.
    ///
    /// [`Write::ARRAY`] is a constant, so exactly one arm of this survives per
    /// element type.
    #[inline]
    pub fn write_slice<T: Write>(&mut self, items: &[T]) {
        match T::ARRAY {
            None => {
                self.begin_generic_array(items.len());
                for item in items {
                    item.write(self);
                }
            }
            Some(h) => {
                self.begin_array(h, items.len());
                T::write_payload(items, self);
            }
        }
    }

    /// Append `items` as a block of payload in one copy.
    ///
    /// The copy behind [`Self::write_slice`], for an implementation of
    /// [`Write::write_payload`] or [`WriteAs::write_payload`] to call once the
    /// header and the count are out. It appends
    /// `items.len() * size_of::<T>()` bytes and nothing else, so it is the
    /// exact counterpart of [`Reader::read_block`].
    ///
    /// # Correctness
    ///
    /// As [`Reader::read_block`], and for the same reason it is not `unsafe`:
    /// the [`NumericBytes`] bound covers `T`'s layout, and the two things it
    /// does not cover are the caller's. The header already written must be the
    /// one [`T::ELEMENT`](NumericBytes::ELEMENT) names, and the host must be
    /// little endian. A big-endian host has to write each element's bytes
    /// reversed instead, which is what every impl in this crate does behind a
    /// `cfg!(target_endian)` test.
    ///
    /// [`Reader::read_block`]: crate::beve::Reader::read_block
    #[inline]
    pub fn write_block<T: NumericBytes>(&mut self, items: &[T]) {
        // SAFETY: read-only reinterpretation of the slice's own
        // `items.len() * size_of::<T>()` initialized bytes, which by the bound
        // are the payload as it should be written.
        let bytes = unsafe {
            core::slice::from_raw_parts(
                items.as_ptr().cast::<u8>(),
                items.len() * Block::<T>::WIDTH,
            )
        };
        self.raw(bytes);
    }

    /// Write a contiguous sequence, each element through an adapter.
    ///
    /// [`Self::write_slice`] over [`WriteAs::ARRAY`] rather than
    /// [`Write::ARRAY`], so an adapted run of numbers stays in the typed array
    /// it belongs in. That constant is what keeps a `Vec<Same>` byte-identical
    /// to the `Vec` it wraps; an adapter that leaves it `None` writes a generic
    /// array, which is a larger document rather than a wrong one.
    #[inline]
    pub fn write_slice_with<A: WriteAs<T>, T>(&mut self, items: &[T]) {
        match A::ARRAY {
            None => {
                self.begin_generic_array(items.len());
                for item in items {
                    A::write(item, self);
                }
            }
            Some(h) => {
                self.begin_array(h, items.len());
                A::write_payload(items, self);
            }
        }
    }

    /// Write a sequence that is not contiguous, each element through an
    /// adapter, always as a generic array.
    ///
    /// [`Self::write_iter`]'s adapted form, and generic for the same reason:
    /// there is no one slice to be a typed array's payload.
    #[inline]
    pub fn write_iter_with<'i, A, T, I>(&mut self, len: usize, items: I)
    where
        A: WriteAs<T>,
        T: 'i + ?Sized,
        I: IntoIterator<Item = &'i T>,
    {
        self.begin_generic_array(len);
        for item in items {
            A::write(item, self);
        }
    }

    /// Write a sequence that is not contiguous, always as a generic array.
    ///
    /// A typed array's payload is one block, so it needs one slice. Sets and
    /// deques have no such slice, and paying to gather one would cost more
    /// than the compactness is worth. Readers accept either form for any
    /// sequence, so this stays interchangeable with [`Self::write_slice`].
    #[inline]
    pub fn write_iter<'i, T, I>(&mut self, len: usize, items: I)
    where
        T: Write + 'i,
        I: IntoIterator<Item = &'i T>,
    {
        self.begin_generic_array(len);
        for item in items {
            item.write(self);
        }
    }

    /// Open a generic array of `len` values, whose elements the caller writes.
    ///
    /// For a sequence whose length is known but whose elements do not come
    /// from one iterator, such as a tuple.
    #[inline]
    pub fn begin_generic_array(&mut self, len: usize) {
        self.push(header::GENERIC_ARRAY);
        self.size(len as u64);
    }

    /// Open a typed array of `len` elements under `header`, which states what
    /// an element is and how wide it is, and whose payload the caller writes.
    ///
    /// That header and the count are the whole preamble, unless
    /// [`Writer::aligned`] is on and the elements are numbers wider than a
    /// byte, which is the rarer case and lives in a function of its own.
    /// Splitting the two keeps this one small
    /// enough to inline into a caller that has the header as a constant, where
    /// the test folds away with everything else.
    ///
    /// `header` must be a typed array's. Anything else is a document no reader
    /// will accept, and under [`Writer::aligned`] it would be wrapped in a
    /// marker as well, so it is refused loudly in a debug build.
    #[inline]
    pub fn begin_typed_array(&mut self, header: u8, len: usize) {
        debug_assert!(
            header::ty(header) == header::TY_TYPED_ARRAY,
            "structio: `begin_typed_array` was given a header that is not a typed array's"
        );
        if self.aligned
            && let Some(width) = padded_width(header)
        {
            self.begin_aligned_array(header, len, width);
            return;
        }
        self.push(header);
        self.size(len as u64);
    }

    /// The aligned half of [`Self::begin_typed_array`]: the marker, the same
    /// header second, the count, and a padding run that lands the payload on a
    /// multiple of `width`.
    ///
    /// The padding's length is stated rather than derived, so a reader steps
    /// over it without having to know where the document began.
    fn begin_aligned_array(&mut self, header: u8, len: usize, width: usize) {
        self.push(header::ALIGNED_ARRAY);
        self.push(header);
        self.size(len as u64);
        // The payload begins one byte past the length byte, so with `o` the
        // offset that byte takes, the padding that lands the payload on a
        // multiple of `width` is `(width - (o + 1) % width) % width`. Since
        // `o % width` is in `0..width`, that is the subtraction below, and it
        // needs no second remainder.
        let pad = width - 1 - self.offset() % width;
        if self.measuring(1 + pad) {
            return;
        }
        self.room(16);
        let len = self.buf.len();
        // SAFETY: `room(16)` guarantees `capacity >= len + 16`, so all sixteen
        // stores stay inside the allocation, and `pad <= width - 1 <= 15`
        // keeps the new length within it. Writing a fixed sixteen and counting
        // only what was announced makes this two stores rather than a call to
        // fill a run whose length is not known until now, exactly as
        // [`Self::size`] writes eight bytes to keep its length a constant.
        unsafe {
            let p = self.buf.as_mut_ptr().add(len);
            p.write(pad as u8);
            core::ptr::write_bytes(p.add(1), 0, 15);
            self.buf.set_len(len + 1 + pad);
        }
    }

    /// Open the array a [`Write::ARRAY`] prefix names.
    ///
    /// One byte is a typed array, which may take the aligned form. Two is the
    /// complex extension, which is not a typed array and has no aligned form
    /// of its own; the specification gives one only to numbers.
    #[inline]
    fn begin_array(&mut self, prefix: &[u8], len: usize) {
        if let [header] = prefix {
            self.begin_typed_array(*header, len);
        } else {
            self.raw(prefix);
            self.size(len as u64);
        }
    }

    /// Write a map as a BEVE object.
    ///
    /// The key type decides the object's header: string keys are the usual
    /// case, and integer keys are stored as integers rather than being
    /// stringified the way JSON has to.
    #[inline]
    pub fn write_keyed<'i, K, V, I>(&mut self, len: usize, entries: I)
    where
        K: crate::beve::impls::ToBeveKey + 'i,
        V: Write + 'i,
        I: IntoIterator<Item = (&'i K, &'i V)>,
    {
        self.push(K::OBJECT);
        self.size(len as u64);
        for (k, v) in entries {
            k.write_key(self);
            v.write(self);
        }
    }

    /// Write a map as a BEVE object, keys and values each through an adapter.
    ///
    /// [`Self::write_keyed`] with both halves adapted. The object header comes
    /// from [`WriteKeyAs::OBJECT`] rather than from the key type, since an
    /// adapter that turns a key into a different kind of key is what the
    /// constant is there for. Name [`Same`](crate::Same) for a half that wants
    /// the type's own impl.
    #[inline]
    pub fn write_keyed_with<'i, KA, VA, K, V, I>(&mut self, len: usize, entries: I)
    where
        KA: WriteKeyAs<K>,
        VA: WriteAs<V>,
        K: 'i + ?Sized,
        V: 'i + ?Sized,
        I: IntoIterator<Item = (&'i K, &'i V)>,
    {
        self.push(KA::OBJECT);
        self.size(len as u64);
        for (k, v) in entries {
            KA::write_key(k, self);
            VA::write(v, self);
        }
    }
}
