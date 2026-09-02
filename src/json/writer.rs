//! The output buffer.
//!
//! [`Writer`] appends straight into a `Vec<u8>`. Hot paths reserve once and
//! then write through a raw pointer into the spare capacity, so the bounds
//! check and capacity check happen once per value rather than once per byte.
//!
//! A writer can also be pointed at an [`io::Write`] sink, in which case the
//! `Vec` becomes a window that is drained as it fills, and the document never
//! exists in memory all at once. See [`Writer::to_sink`].

use std::io;
use std::marker::PhantomData;

use crate::json::traits::{Write, WriteArray, WriteAs, WriteKeyAs, WriteObject};
use crate::num::atof::is_number;
use crate::num::dtoa::{MAX_FLOAT_BYTES, write_f32, write_f64};
use crate::num::itoa::{MAX_INT_DIGITS, write_u64};
use crate::options::{Options, Standard};
use crate::swar::{escape_mask, first_match, load_u64, needs_escape};

/// How much a sink-backed writer buffers before draining.
pub const DEFAULT_SINK_BUFFER: usize = 8 * 1024;

/// The complete `"KEY":` prefix of a JSON member.
///
/// `N` must be the key's length plus the three punctuation bytes; the macro
/// derives it from exactly that. The JSON counterpart of
/// [`beve::header::encode_key`](crate::beve::header::encode_key), and needed
/// for the same reason: assembling the prefix at compile time makes writing a
/// member one copy of one constant.
///
/// A declaration that spells its key out gets this prefix from `concat!`
/// instead, which cannot be given a computed string. This is the path a
/// [case rule](crate::case) takes, and the key it is handed comes from a Rust
/// identifier, so there is nothing in it for JSON to escape.
pub const fn quoted_key<const N: usize>(key: &str) -> [u8; N] {
    let bytes = key.as_bytes();
    assert!(
        N == bytes.len() + 3,
        "structio: `quoted_key` was given a length that is not the key's plus its punctuation"
    );
    let mut out = [0u8; N];
    out[0] = b'"';
    let mut i = 0;
    while i < bytes.len() {
        out[1 + i] = bytes[i];
        i += 1;
    }
    out[N - 2] = b'"';
    out[N - 1] = b':';
    out
}

/// Accumulates JSON output.
///
/// The lifetime is the borrow of an [`io::Write`] sink, and is `'static` for
/// the ordinary in-memory writers built by [`Writer::new`] and friends.
///
/// `O` is the [write policy](crate::Options), which decides at compile time
/// whether the output is indented and whether null members are left out. It
/// defaults to [`Standard`], though a constructor cannot infer from that, so
/// build one as `Writer::<Standard>::new()`. Trait implementations take
/// `&mut Writer<'_, O>` and stay generic over it.
pub struct Writer<'a, O: Options = Standard> {
    buf: Vec<u8>,
    /// Highest length an append may reach on the fast path.
    ///
    /// Without a sink this is exactly `buf.capacity()`, so testing against it
    /// is the same capacity test `Vec` performs anyway. With one it is the
    /// lesser of the capacity and the drain threshold, which folds "is there
    /// room" and "is it time to drain" into that one test. Draining therefore
    /// costs the write path nothing at all: no extra compare, and no drain
    /// call sites for the container loops to carry.
    limit: usize,
    /// Buffer size to drain back down to, or `usize::MAX` with no sink, which
    /// makes `limit` collapse to the capacity and every drain a no-op.
    threshold: usize,
    sink: Option<Sink<'a>>,
    /// Where this writer's own output begins.
    ///
    /// Zero for every writer that starts from an empty buffer, and the length
    /// of the buffer handed to [`Writer::appending`] otherwise. Those leading
    /// bytes are the one part of the buffer this writer did not produce, and
    /// so the one part whose UTF-8 is not known by construction;
    /// [`Writer::into_string`] is where that matters, and slices the buffer
    /// here. That index holds: the only append path that shortens the buffer
    /// is the closing byte overwriting a container's last comma, and a
    /// container always wrote its opening byte first, so a writer never cuts
    /// back past its own first byte.
    text_from: usize,
    /// Nesting depth, for indentation. Only ever read under
    /// [`Options::PRETTY`], and only ever updated by a container that breaks
    /// its contents across lines, so a compact writer does not track it and an
    /// array written inline claims no level it would not use.
    depth: usize,
    /// `fn() -> O` rather than `O`, so the writer's auto traits depend on what
    /// it holds rather than on a policy type it never contains.
    options: PhantomData<fn() -> O>,
}

/// The sink half of a streaming writer.
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
    /// A sink writer that is dropped without [`Writer::finish`] silently
    /// truncates its output, and reports no error for it. Nothing in the type
    /// system prevents that, so it is at least loud in a debug build.
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
    /// [`Self::from_vec`] discards the contents; this keeps them and writes
    /// past them. A document that has to sit behind something -- a protocol
    /// header, or the entries already written into a listing -- then costs one
    /// buffer rather than a second buffer and a copy out of it.
    /// [`json::append`](crate::json::append) is this with the writer kept out
    /// of sight, and [`beve::Writer::appending`](crate::beve::Writer::appending)
    /// is the same constructor on the other format.
    ///
    /// The bytes in front are not examined and need not be text, since what
    /// comes back from [`Self::into_vec`] is bytes. They are checked, rather
    /// than trusted, by [`Self::into_string`], which is what makes appending
    /// onto the bytes of a `String` sound:
    ///
    /// ```
    /// use structio::Standard;
    /// use structio::json::{Write, Writer};
    ///
    /// let out = String::from("[1,2,");
    /// let mut w = Writer::<Standard>::appending(out.into_bytes());
    /// 3.write(&mut w);
    /// let mut out = w.into_string();
    /// out.push(']');
    ///
    /// assert_eq!(out, "[1,2,3]");
    /// ```
    #[inline]
    pub fn appending(buf: Vec<u8>) -> Self {
        Writer {
            limit: buf.capacity(),
            threshold: usize::MAX,
            text_from: buf.len(),
            buf,
            sink: None,
            depth: 0,
            options: PhantomData,
        }
    }
}

impl<'a, O: Options> Writer<'a, O> {
    /// Write through to `out`, buffering [`DEFAULT_SINK_BUFFER`] bytes at a
    /// time.
    ///
    /// The document is drained as it is produced, so peak memory is the buffer
    /// plus the largest single scalar written, not the size of the output.
    /// [`Writer::finish`] must be called to flush the tail and report any I/O
    /// error; [`crate::to_writer`] does both.
    #[inline]
    pub fn to_sink(out: &'a mut dyn io::Write) -> Self {
        Self::to_sink_with_capacity(out, DEFAULT_SINK_BUFFER)
    }

    /// [`Writer::to_sink`] with an explicit buffer size.
    ///
    /// `capacity` is clamped up to one byte: draining always retains the last
    /// byte written, because it may still be a trailing comma awaiting
    /// overwrite by a closing brace.
    pub fn to_sink_with_capacity(out: &'a mut dyn io::Write, capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let buf = Vec::with_capacity(capacity);
        Writer {
            limit: buf.capacity().min(capacity),
            threshold: capacity,
            text_from: 0,
            buf,
            sink: Some(Sink {
                out,
                err: None,
                finished: false,
            }),
            depth: 0,
            options: PhantomData,
        }
    }

    /// The bytes written so far, or with a sink, the bytes not yet drained.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Take the output.
    ///
    /// Everything written is valid UTF-8 by construction: string contents come
    /// from `&str`, numbers and escapes are ASCII, and [`Writer::push`] rejects
    /// non-ASCII bytes.
    ///
    /// With a sink this returns only the undrained tail. Use
    /// [`Writer::finish`] instead.
    ///
    /// # Panics
    ///
    /// If the writer was built by [`Writer::appending`] over bytes that are
    /// not valid UTF-8. Those are the only bytes in the buffer this writer did
    /// not produce, so they are the only ones it has to check; a binary prefix
    /// is a perfectly good thing to append JSON behind, but it cannot come
    /// back out as a `String`. Take [`Writer::into_vec`] there instead.
    #[inline]
    pub fn into_string(self) -> String {
        // The scan is over the prefix alone, not the document, and a writer
        // that began at an empty buffer has no prefix to scan. Well-formed
        // UTF-8 either side of the join makes the whole buffer well-formed,
        // since the prefix ends on a character boundary by being whole.
        assert!(
            core::str::from_utf8(&self.buf[..self.text_from]).is_ok(),
            "structio: `Writer::appending` was given bytes that are not UTF-8, so `into_string` \
             cannot hand back a `String`; use `into_vec`"
        );
        debug_assert!(core::str::from_utf8(&self.buf).is_ok());
        // SAFETY: every append path preserves UTF-8. `write_str` and `raw` copy
        // from a `&str`; numbers, escapes, and structural bytes are ASCII; and
        // `push`, the only byte-level entry point safe code can reach, asserts
        // its argument is ASCII. Draining is the one thing that removes a
        // prefix rather than adding a suffix, and it cuts only on a character
        // boundary, so what is left is still whole characters. Bytes the
        // writer was handed rather than wrote have just been checked.
        unsafe { String::from_utf8_unchecked(self.buf) }
    }

    #[inline]
    pub fn into_vec(self) -> Vec<u8> {
        self.buf
    }

    /// Bytes currently buffered.
    ///
    /// With a sink this counts only what has not been drained, matching
    /// [`Writer::as_bytes`]. A running total of the whole document is not
    /// offered because a failed drain would make it a lie. With
    /// [`Writer::appending`] it counts the bytes handed in as well, those
    /// being in the buffer.
    #[inline]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Drain the remaining bytes to the sink and report the first I/O error.
    ///
    /// Without a sink this is `Ok(())` and does nothing.
    ///
    /// By value on purpose. A sink writer that keeps going after its tail has
    /// been flushed would emit a second document's worth of bytes into the
    /// middle of the first, and asking twice whether the write succeeded has
    /// no second answer worth giving.
    pub fn finish(mut self) -> io::Result<()> {
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

    /// Guarantee room for `n` more bytes, draining or growing if there is not.
    ///
    /// Every append goes through here, and on the fast path it is the single
    /// compare `Vec` would have made on its own. That is the whole reason the
    /// streaming writer is free: the batch path runs the same instructions it
    /// did before draining existed.
    #[inline(always)]
    fn room(&mut self, n: usize) {
        if self.buf.len() + n > self.limit {
            self.spill(n);
        }
    }

    /// The out-of-line half of [`Self::room`]: make room, then restate the
    /// limit.
    ///
    /// A sink is emptied first, since that is usually enough and costs no
    /// memory. Only a value too large for the whole buffer, or a writer with
    /// no sink, reaches the allocation.
    #[cold]
    fn spill(&mut self, n: usize) {
        self.drain();
        if self.buf.len() + n > self.buf.capacity() {
            self.buf.reserve(n);
        }
        self.relimit();
    }

    /// Recompute the fast-path limit after the capacity may have moved.
    ///
    /// This is the only place `limit` is assigned outside construction, and
    /// the `min` is what keeps `limit <= capacity`, which every `set_len` in
    /// this file depends on.
    #[inline]
    fn relimit(&mut self) {
        self.limit = self.buf.capacity().min(self.threshold);
    }

    /// Hand the front of the buffer to the sink, keeping a tail back.
    ///
    /// Two things decide where to cut.
    ///
    /// The last byte is always retained, because [`Self::overwrite_last`]
    /// turns a member's trailing comma into the closing brace and that comma
    /// is always the most recent byte written. Keeping one byte back is
    /// exactly enough to leave that rewrite available.
    ///
    /// The cut is then walked back to a character boundary. The buffer holds
    /// whole characters, but the last byte alone may be the tail of one, and
    /// [`Self::into_string`] converts what remains without revalidating. A
    /// sink being handed text gets whole characters out of the same rule.
    /// Continuation bytes come in runs of at most three, so this is bounded.
    fn drain(&mut self) {
        let n = self.buf.len();
        if n < 2 || self.sink.is_none() {
            // Nothing to hand over that is not the retained byte, or nowhere
            // to hand it.
            return;
        }
        let mut cut = n - 1;
        while cut > 0 && (self.buf[cut] & 0xC0) == 0x80 {
            cut -= 1;
        }
        if cut == 0 {
            // The whole buffer is one character, so there is no cut that both
            // keeps a byte back and lands on a boundary.
            return;
        }
        let sink = self.sink.as_mut().expect("checked above");
        if sink.err.is_none()
            && let Err(e) = sink.out.write_all(&self.buf[..cut])
        {
            sink.err = Some(e);
        }
        // Discard even on failure, so a broken sink does not turn into an
        // unbounded allocation.
        self.buf.copy_within(cut.., 0);
        self.buf.truncate(n - cut);
    }

    /// Append one ASCII byte.
    ///
    /// # Panics
    ///
    /// If `b` is not ASCII. [`Writer::into_string`] converts without
    /// re-validating, so the buffer has to stay valid UTF-8, and this is the
    /// only way safe code could break that. At every internal call site `b` is
    /// a literal, so the check folds away.
    #[inline(always)]
    pub fn push(&mut self, b: u8) {
        assert!(
            b.is_ascii(),
            "structio: Writer::push requires an ASCII byte"
        );
        self.room(1);
        let len = self.buf.len();
        // SAFETY: `room(1)` leaves `capacity >= len + 1`, so the byte lands
        // inside the allocation and the new length is within capacity.
        unsafe {
            self.buf.as_mut_ptr().add(len).write(b);
            self.buf.set_len(len + 1);
        }
    }

    /// Append a string verbatim, without quoting or escaping it.
    ///
    /// Whatever `s` holds becomes part of the document as written, so keeping
    /// the result valid JSON is the caller's job. Emitting a number literal is
    /// a supported use of this, and
    /// [`write_number_str`](Self::write_number_str) is that use with the
    /// literal checked.
    #[inline(always)]
    pub fn raw(&mut self, s: &str) {
        self.append(s.as_bytes());
    }

    /// Append bytes verbatim. Internal, because the caller is responsible for
    /// keeping the buffer valid UTF-8.
    #[inline(always)]
    pub(crate) fn raw_bytes(&mut self, b: &[u8]) {
        self.append(b);
    }

    /// Copy a run of bytes in, draining or growing first if it does not fit.
    #[inline(always)]
    fn append(&mut self, bytes: &[u8]) {
        self.room(bytes.len());
        let len = self.buf.len();
        // SAFETY: `room` leaves `capacity >= len + bytes.len()`, and `bytes`
        // is a distinct allocation from the buffer.
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.buf.as_mut_ptr().add(len),
                bytes.len(),
            );
            self.buf.set_len(len + bytes.len());
        }
    }

    /// Overwrite the trailing comma with a container's closing byte.
    ///
    /// Only [`Self::close_over_comma`] may call this, and only once it has
    /// seen that the last byte really is a comma. That matters for more than
    /// tidiness: the buffer is handed out by [`Self::into_string`] without
    /// revalidation, so overwriting a byte that turned out to be part of a
    /// character would make a `String` that is not UTF-8.
    #[inline(always)]
    fn overwrite_last(&mut self, b: u8) {
        let n = self.buf.len();
        debug_assert!(n > 0);
        self.buf[n - 1] = b;
    }

    /// Break the line and indent to the current depth, under
    /// [`Options::PRETTY`] and not otherwise.
    ///
    /// The whole body is behind a constant, so a compact writer compiles this
    /// to nothing at all and never loads the depth it would have read.
    #[inline]
    pub(crate) fn line(&mut self) {
        if O::PRETTY {
            self.push(b'\n');
            self.spaces(self.depth * O::INDENT);
        }
    }

    /// Append `n` spaces, a chunk at a time.
    ///
    /// A constant run copied out of read-only memory, rather than a fill
    /// through spare capacity, because indentation only exists on the pretty
    /// path and that path has already accepted a much larger cost in bytes
    /// written. Keeping it out of the `unsafe` families is worth more here
    /// than saving a `memcpy` bound.
    fn spaces(&mut self, mut n: usize) {
        const RUN: &[u8; 32] = &[b' '; 32];
        while n > 0 {
            let take = n.min(RUN.len());
            self.append(&RUN[..take]);
            n -= take;
        }
    }

    /// The break before an array element: a line of its own, or the space
    /// after the comma that stands in for one under an inline-array policy.
    ///
    /// The space goes before the element rather than after the comma because
    /// the comma is what the closing bracket overwrites. An element that
    /// turns out to be the last one leaves `,` as the final byte either way,
    /// so there is never a trailing space to take back. The byte before an
    /// element is `[` when it is the first and `,` when it is not, which makes
    /// the comma the whole test, read back off the buffer the way
    /// [`Self::close_over_comma`] reads it to tell an empty container from a
    /// full one.
    #[inline]
    pub(crate) fn item(&mut self) {
        if O::NEW_LINES_IN_ARRAYS {
            self.line();
        } else if O::PRETTY && self.buf.last() == Some(&b',') {
            self.push(b' ');
        }
    }

    /// Write the `:` between a key and its value, spaced under
    /// [`Options::PRETTY`].
    #[inline]
    pub(crate) fn colon(&mut self) {
        self.push(b':');
        if O::PRETTY {
            self.push(b' ');
        }
    }

    /// Whether a container written with `bracket` puts its contents on lines
    /// of their own.
    ///
    /// An object always does under [`Options::PRETTY`]; an array does unless
    /// [`Options::NEW_LINES_IN_ARRAYS`] is off, in which case it also costs no
    /// level of indentation, nothing being indented against it. Every call
    /// site passes a bracket literal, so this is a constant where it is asked.
    #[inline(always)]
    fn breaks_lines(bracket: u8) -> bool {
        O::PRETTY && (O::NEW_LINES_IN_ARRAYS || matches!(bracket, b'{' | b'}'))
    }

    /// Open a container: its bracket, and a level of indentation.
    ///
    /// Paired with [`Self::close`], and the pairing is the point. Writing the
    /// bracket and adjusting the depth used to be separate steps, which meant a
    /// hand-built container could write one and forget the other; two of them
    /// did, and produced JSON indented against the wrong level. There is now no
    /// way to write the bracket without the depth following it.
    #[inline]
    pub(crate) fn open(&mut self, bracket: u8) {
        self.push(bracket);
        if Self::breaks_lines(bracket) {
            self.depth += 1;
        }
    }

    /// Close a container opened by [`Self::open`]: the level, then the bracket
    /// over the trailing comma.
    ///
    /// Every item must have written a trailing comma, which is the separator
    /// this overwrites and, when the contents go on lines of their own, the
    /// marker that says the container had anything in it. A last item that
    /// omits its comma leaves the bracket with nothing to replace, so it lands
    /// against that item instead of on its own line.
    #[inline]
    pub(crate) fn close(&mut self, bracket: u8) {
        if Self::breaks_lines(bracket) {
            self.depth -= 1;
        }
        self.close_over_comma(bracket);
    }

    /// Begin a member: its line, its pre-quoted `"key":`, and the space after
    /// the colon that indentation asks for.
    #[inline(always)]
    pub(crate) fn key(&mut self, prefix: &str) {
        self.line();
        self.raw(prefix);
        if O::PRETTY {
            self.push(b' ');
        }
    }

    /// Append the first `n` bytes of a fixed-size buffer.
    ///
    /// The copy is always `MAX` bytes, a compile-time constant, so it lowers to
    /// a couple of wide stores instead of a call to `memcpy` with a runtime
    /// length. The tail beyond `n` lands in spare capacity and is discarded by
    /// the `set_len`. This is the same trick Glaze's `dump` uses, and it is
    /// worth a surprising amount on short tokens like `true` and small
    /// integers.
    #[inline(always)]
    pub(crate) fn append_fixed<const MAX: usize>(&mut self, src: &[u8; MAX], n: usize) {
        // Not a `debug_assert`: `n` crosses a module boundary from `num`, and it
        // is the sole guard on the `set_len` below. At the literal call sites it
        // folds away; on the number paths it is one predictable compare.
        assert!(n <= MAX);
        self.room(MAX);
        let len = self.buf.len();
        // SAFETY: `room(MAX)` guarantees `capacity >= len + MAX`, so writing
        // `MAX` bytes at `len` stays inside the allocation, and `n <= MAX` keeps
        // the new length within capacity. Every byte below `len + n` has just
        // been written.
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), self.buf.as_mut_ptr().add(len), MAX);
            self.buf.set_len(len + n);
        }
    }

    // -----------------------------------------------------------------------
    // Structural
    // -----------------------------------------------------------------------

    /// Write a struct as a JSON object.
    ///
    /// Members are written with an unconditional trailing comma, and the last
    /// one is overwritten with `}`. That removes the per-field "am I first"
    /// branch from the inner loop entirely.
    ///
    /// The overwrite checks for that comma rather than assuming it.
    /// [`WriteObject`] is a safe trait that anyone may implement, so nothing
    /// guarantees `write_fields` wrote what it was asked to, and the buffer is
    /// handed out by [`Writer::into_string`] without revalidation.
    #[inline]
    pub fn write_object<T: WriteObject>(&mut self, value: &T) {
        self.open(b'{');
        value.write_fields(self);
        self.close(b'}');
    }

    /// Write one `"key":value,` member. `prefix` is the pre-quoted key with its
    /// colon, built at compile time by the macro.
    ///
    /// Under [`Options::SKIP_NULL`] a member holding nothing is not written at
    /// all, key included. The test is a constant plus a call that is `false`
    /// for all but a handful of types, so a policy that does not ask for it
    /// pays nothing and one that does pays a predictable branch on a value
    /// already in hand.
    #[inline(always)]
    pub fn member<T: Write + ?Sized>(&mut self, prefix: &str, value: &T) {
        if O::SKIP_NULL && value.is_null() {
            return;
        }
        self.key(prefix);
        value.write(self);
        self.push(b',');
    }

    /// Write one `"key":value,` member, the value through an adapter.
    ///
    /// [`Self::member`] for a field whose declaration named an adapter, down to
    /// the trailing comma and to [`Options::SKIP_NULL`], which asks
    /// [`WriteAs::is_null`] rather than the value itself. A member that writes
    /// itself would escape both.
    ///
    /// `A` appears in no argument, so it is always turned up explicitly:
    /// `w.member_with::<Millis, _>(prefix, value)`.
    #[inline(always)]
    pub fn member_with<A: WriteAs<T>, T: ?Sized>(&mut self, prefix: &str, value: &T) {
        if O::SKIP_NULL && A::is_null(value) {
            return;
        }
        self.key(prefix);
        A::write(value, self);
        self.push(b',');
    }

    /// Write an enum variant that carries a value: `{"Name":value}`.
    ///
    /// `prefix` is the pre-quoted name with its colon, built at compile time
    /// by the macro, exactly as [`Self::member`] takes one. A variant carrying
    /// nothing is not written here at all: it is its own name, so it goes
    /// through [`Self::write_str`].
    ///
    /// [`Options::SKIP_NULL`] deliberately does not reach here. Dropping the
    /// member would leave `{}`, which names no variant and so is not a smaller
    /// spelling of this value but a different one.
    #[inline]
    pub fn write_tagged<T: Write + ?Sized>(&mut self, prefix: &str, value: &T) {
        self.open(b'{');
        self.key(prefix);
        value.write(self);
        self.push(b',');
        self.close(b'}');
    }

    /// Write a struct as a JSON array.
    ///
    /// The bracket counterpart of [`Self::write_object`], down to the trailing
    /// comma each element writes and the closing bracket that overwrites the
    /// last of them.
    #[inline]
    pub fn write_array<T: WriteArray>(&mut self, value: &T) {
        self.open(b'[');
        value.write_elements(self);
        self.close(b']');
    }

    /// Write one `value,` element.
    ///
    /// [`Options::SKIP_NULL`] deliberately does not reach here. Dropping a null
    /// from a sequence would shorten it and shift every index after it, which
    /// is a change to the data rather than to its presentation.
    #[inline(always)]
    pub fn element<T: Write + ?Sized>(&mut self, value: &T) {
        self.item();
        value.write(self);
        self.push(b',');
    }

    /// Write a sequence as a JSON array.
    #[inline]
    pub fn write_seq<'i, T, I>(&mut self, items: I)
    where
        T: Write + 'i,
        I: IntoIterator<Item = &'i T>,
    {
        self.open(b'[');
        for value in items {
            self.item();
            value.write(self);
            self.push(b',');
        }
        self.close(b']');
    }

    /// Write a sequence as a JSON array, each element through an adapter.
    ///
    /// [`Self::write_seq`] for elements the adapter describes rather than their
    /// own [`Write`] impl. It is what `Vec<A>`'s [`WriteAs`] is built on, and
    /// it is public so that an adapter defined outside this crate can write a
    /// sequence whose elements another adapter describes; the bracket methods
    /// are `pub(crate)`, so `write_seq` and this are the two ways to a JSON
    /// array. Elements described by neither trait still have to be turned into
    /// something that is.
    #[inline]
    pub fn write_seq_with<'i, A, T, I>(&mut self, items: I)
    where
        A: WriteAs<T>,
        T: 'i + ?Sized,
        I: IntoIterator<Item = &'i T>,
    {
        self.open(b'[');
        for value in items {
            self.item();
            A::write(value, self);
            self.push(b',');
        }
        self.close(b']');
    }

    /// Write a map as a JSON object.
    ///
    /// Keys go through [`ToJsonKey`](crate::json::ToJsonKey), so numeric keys come
    /// out quoted, which is the only form JSON has for an object key.
    #[inline]
    pub fn write_keyed<'i, K, V, I>(&mut self, entries: I)
    where
        K: crate::json::impls::ToJsonKey + 'i,
        V: Write + 'i,
        I: IntoIterator<Item = (&'i K, &'i V)>,
    {
        self.open(b'{');
        for (k, v) in entries {
            self.line();
            k.write_key(self);
            self.colon();
            v.write(self);
            self.push(b',');
        }
        self.close(b'}');
    }

    /// Write a map as a JSON object, keys and values each through an adapter.
    ///
    /// [`Self::write_keyed`] with both halves adapted, which is what
    /// `HashMap<KA, VA>`'s [`WriteAs`] is built on. Name
    /// [`Same`](crate::Same) for a half that wants the type's own impl.
    #[inline]
    pub fn write_keyed_with<'i, KA, VA, K, V, I>(&mut self, entries: I)
    where
        KA: WriteKeyAs<K>,
        VA: WriteAs<V>,
        K: 'i + ?Sized,
        V: 'i + ?Sized,
        I: IntoIterator<Item = (&'i K, &'i V)>,
    {
        self.open(b'{');
        for (k, v) in entries {
            self.line();
            KA::write_key(k, self);
            self.colon();
            VA::write(v, self);
            self.push(b',');
        }
        self.close(b'}');
    }

    /// Close a container whose members each wrote a trailing comma.
    ///
    /// Turns the final comma into `close`, or appends `close` when nothing was
    /// written. That is the same decision an "is this the first member?" flag
    /// would make, read back off the buffer instead of carried through the loop.
    ///
    /// Private, and reached only through [`Self::close`], so that closing a
    /// container cannot be spelled without the matching depth adjustment.
    /// `close` also reaches [`Self::overwrite_last`] unchecked, and the buffer
    /// must stay ASCII for [`Self::into_string`].
    #[inline(always)]
    fn close_over_comma(&mut self, close: u8) {
        debug_assert!(close.is_ascii());
        if Self::breaks_lines(close) {
            // The comma has to go before the closing line breaks, so it is
            // dropped rather than overwritten and the bracket lines up under
            // the line the container opened on. An empty container never wrote
            // one, and stays on a single line as `{}` or `[]`.
            if self.buf.last() == Some(&b',') {
                self.buf.pop();
                self.line();
            }
            self.push(close);
        } else if self.buf.last() == Some(&b',') {
            self.overwrite_last(close);
        } else {
            self.push(close);
        }
    }

    // -----------------------------------------------------------------------
    // Scalars
    // -----------------------------------------------------------------------

    #[inline(always)]
    pub fn write_bool(&mut self, v: bool) {
        // Select a pointer, then copy a constant eight bytes either way.
        if v {
            self.append_fixed(b"true\0\0\0\0", 4);
        } else {
            self.append_fixed(b"false\0\0\0", 5);
        }
    }

    #[inline(always)]
    pub fn write_null(&mut self) {
        self.append_fixed(b"null\0\0\0\0", 4);
    }

    #[inline]
    pub fn write_u64(&mut self, v: u64) {
        let mut tmp = [0u8; MAX_INT_DIGITS];
        let n = write_u64(v, &mut tmp);
        self.append_fixed(&tmp, n);
    }

    #[inline]
    pub fn write_i64(&mut self, v: i64) {
        if v < 0 {
            self.push(b'-');
            // Negate through `u64` so `i64::MIN` does not overflow.
            self.write_u64((v as u64).wrapping_neg());
        } else {
            self.write_u64(v as u64);
        }
    }

    /// Write a `u128`.
    ///
    /// Values past `u64::MAX` are rare enough that the wide path is a plain
    /// loop rather than a table walk.
    pub fn write_u128(&mut self, v: u128) {
        if let Ok(small) = u64::try_from(v) {
            return self.write_u64(small);
        }
        let mut tmp = [0u8; 39];
        let mut p = tmp.len();
        let mut v = v;
        while v > 0 {
            p -= 1;
            tmp[p] = b'0' + (v % 10) as u8;
            v /= 10;
        }
        self.append(&tmp[p..]);
    }

    /// Write an `i128`, also used for the quoted-integer key path.
    pub fn write_i128_raw(&mut self, v: i128) {
        if v < 0 {
            self.push(b'-');
            self.write_u128((v as u128).wrapping_neg());
        } else {
            self.write_u128(v as u128);
        }
    }

    /// Write an `f64`. NaN and infinity have no JSON form, so they are written
    /// as `null`, matching Glaze.
    #[inline]
    pub fn write_f64(&mut self, v: f64) {
        let mut tmp = [0u8; MAX_FLOAT_BYTES];
        match write_f64(v, &mut tmp) {
            Some(n) => self.append_fixed(&tmp, n),
            None => self.write_null(),
        }
    }

    #[inline]
    pub fn write_f32(&mut self, v: f32) {
        let mut tmp = [0u8; MAX_FLOAT_BYTES];
        match write_f32(v, &mut tmp) {
            Some(n) => self.append_fixed(&tmp, n),
            None => self.write_null(),
        }
    }

    /// Write a number already in its JSON form.
    ///
    /// The other half of
    /// [`Parser::read_number_str`](crate::json::Parser::read_number_str),
    /// which is where the case for the pair is written out.
    ///
    /// # Panics
    ///
    /// Under `debug_assertions`, if `s` is not one JSON number literal. That
    /// is a bug in the caller rather than a condition to handle: the document
    /// is already being written, so there is nowhere for an error to go and
    /// nothing to do but publish something no reader will accept. Release
    /// builds append `s` unchecked, as [`raw`](Self::raw) does.
    ///
    /// ```
    /// use structio::{Standard, json::Writer};
    ///
    /// let mut w = Writer::<Standard>::new();
    /// w.write_number_str("-1.2345678901234567890123e400");
    /// assert_eq!(w.into_string(), "-1.2345678901234567890123e400");
    /// ```
    #[inline]
    pub fn write_number_str(&mut self, s: &str) {
        debug_assert!(
            is_number(s),
            "structio: Writer::write_number_str requires a JSON number literal"
        );
        self.raw(s);
    }

    // -----------------------------------------------------------------------
    // Strings
    // -----------------------------------------------------------------------

    /// Write a quoted, escaped JSON string.
    ///
    /// The common case is a string with nothing to escape, so the scan runs
    /// eight bytes at a time and copies whole runs between escapes rather than
    /// testing byte by byte.
    pub fn write_str(&mut self, s: &str) {
        let bytes = s.as_bytes();
        // One hint for the whole string, so the ordinary no-escape case makes
        // a single trip through `spill`.
        self.room(bytes.len() + 2);
        self.push(b'"');

        let mut start = 0;
        let mut i = 0;
        let n = bytes.len();

        while i < n {
            // Bulk scan: skip eight bytes at a time while none need escaping.
            if i + 8 <= n {
                // SAFETY: `i + 8 <= n`, so the read is in bounds.
                let m = escape_mask(unsafe { load_u64(bytes, i) });
                if m == 0 {
                    i += 8;
                    continue;
                }
                // The mask already located the byte, so jump to it rather than
                // walking the chunk again one byte at a time.
                i += first_match(m);
            } else if !needs_escape(bytes[i]) {
                i += 1;
                continue;
            }
            let c = bytes[i];
            self.append(&bytes[start..i]);
            self.write_escape(c);
            i += 1;
            start = i;
        }
        self.append(&bytes[start..]);
        self.push(b'"');
    }

    #[cold]
    fn write_escape(&mut self, c: u8) {
        match c {
            b'"' => self.raw_bytes(b"\\\""),
            b'\\' => self.raw_bytes(b"\\\\"),
            0x08 => self.raw_bytes(b"\\b"),
            0x0C => self.raw_bytes(b"\\f"),
            b'\n' => self.raw_bytes(b"\\n"),
            b'\r' => self.raw_bytes(b"\\r"),
            b'\t' => self.raw_bytes(b"\\t"),
            _ => {
                // Remaining control characters have no short form.
                const HEX: &[u8; 16] = b"0123456789abcdef";
                self.raw_bytes(b"\\u00");
                self.push(HEX[(c >> 4) as usize]);
                self.push(HEX[(c & 0xF) as usize]);
            }
        }
    }
}
