//! The BEVE input cursor.
//!
//! BEVE is self-describing and length-prefixed, so reading is a walk rather
//! than a scan: every value announces its type in one byte and its extent in
//! the bytes that follow. There is no whitespace to skip, no escape to undo,
//! and no delimiter to search for.
//!
//! # Reading is lenient about width, strict about kind
//!
//! A producer writes a number at the width its own type had. A `u16` field
//! here will therefore meet a `u8`, a `u32`, or an `i64` on the wire depending
//! on what the other side declared, and refusing those would make the format
//! useless across languages. So any integer header satisfies any integer
//! field, with the value range-checked into the target, and any number header
//! satisfies a float field. What is *not* accepted is a different kind: a
//! string where a number was asked for is an error, never a conversion.
//!
//! The same leniency covers arrays. A sequence accepts a typed array or a
//! generic one, whichever the producer chose, and a typed array of one width
//! read into a `Vec` of another is widened element by element.
//!
//! # Implied headers
//!
//! A typed array stores one header for the whole run, so its elements carry
//! none of their own. Rather than give every scalar reader a second entry
//! point, the array driver *installs* the header its next element would have
//! had, and `Reader::head` hands that out instead of consuming a byte. A
//! packed boolean array installs a `true` or `false` header per index and
//! never moves the cursor at all until the run is done.
//!
//! This is why a `Vec<String>` reading a boolean array reports "expected a
//! string" rather than something stranger: the element reader sees a boolean
//! header, exactly as it would outside an array.
//!
//! # One set of extents, several walks
//!
//! Reading into a type, stepping over a value, seeking to one, and
//! [transcoding one to JSON](crate::transcode) all locate a value with the same
//! primitives, which is why those are shared rather than private. A walk that
//! worked an extent out for itself would eventually disagree with the others,
//! and the one that disagreed would be whichever was least used.

use core::marker::PhantomData;

use crate::beve::header::{self, byte_width, decode_size};
use crate::beve::impls::{Block, NumericBytes};
use crate::beve::traits::{Read, ReadArray, ReadAs, ReadEnum, ReadInternallyTagged, ReadObject};
use crate::error::{ErrorCode, PResult};
use crate::num::atoi::parse_int_text;
use crate::options::{Options, Standard};
use crate::traits::{Fields, resolve_key, resolve_variant};

/// Deepest nesting accepted, so a hostile document cannot exhaust the stack.
pub const MAX_DEPTH: u32 = 256;

/// A BEVE object key, in whichever of the three forms the object declared.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key<'de> {
    Str(&'de str),
    Signed(i128),
    Unsigned(u128),
}

/// The members between an object's first key and a tag that was not it:
/// `count` of them starting at `start`, belonging to the object at `depth`.
#[derive(Clone, Copy)]
struct Deferred {
    start: usize,
    count: usize,
    depth: u32,
}

/// The most memory a reservation made on a count from the wire may claim
/// before the elements have been read. A count the input can hold is still
/// only a count: a hostile document one megabyte long can claim a million
/// elements of a type that is a kilobyte each.
const RESERVE_LIMIT: usize = 1 << 20;

/// How many `T` to reserve on a count of `n` from the wire.
///
/// The count a [`read_seq_counted`](Reader::read_seq_counted) or
/// [`read_map_counted`](Reader::read_map_counted) hands over is bounded
/// by the bytes the input has left, which bounds how many elements it can
/// hold but not what each costs in memory. This clips it so that no more than
/// a megabyte is reserved on the count's word; an honest document of that
/// many elements grows from there in the ordinary way, and a dishonest one is
/// found out having wasted at most that. A zero-sized `T` costs nothing to
/// reserve and is passed through.
#[inline]
pub fn cautious<T>(n: usize) -> usize {
    match core::mem::size_of::<T>() {
        0 => n,
        w => n.min(RESERVE_LIMIT / w),
    }
}

/// Which of the three shapes a typed array's payload has, with its preamble
/// already consumed.
///
/// The distinction the header draws is between payloads that are walked
/// differently, not between element types: the aligned form collapses into
/// [`Typed::Fixed`] here because past its preamble that is exactly what it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Typed {
    /// One boolean per bit, low bit first.
    Bools(usize),
    /// Each element its own length and text, so the run has to be walked.
    Strings(usize),
    /// A contiguous block, addressable by multiplying. Carries the header its
    /// elements derive from, which for the aligned form is the inner one.
    Fixed(u8, usize),
}

/// An integer read at whatever width it was stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Int {
    Signed(i128),
    Unsigned(u128),
}

/// Walks a BEVE document.
///
/// `O` is the [read policy](crate::Options), named once at construction and
/// inferred everywhere after. It holds nothing and is never constructed; it is
/// read through `O::CONSTANT` where a setting is consulted. It defaults to
/// [`Standard`] where the type is written out.
pub struct Reader<'de, O: Options = Standard> {
    data: &'de [u8],
    pos: usize,
    depth: u32,
    /// The key to attach to the failure this read is about to return. See
    /// [`set_error_key`](Reader::set_error_key).
    error_key: Option<&'static str>,
    /// The header the next value must be read with, when it carries none of
    /// its own. `Some` only while the cursor stands on the value it was
    /// installed for. See the module docs.
    implied: Option<u8>,
    /// Where the value `implied` was last installed for begins, and that
    /// header, kept after the value's read has taken it so that
    /// [`rewind`](Reader::rewind) back onto the value can put it back, and
    /// dropped by a rewind anywhere else. See [`implying`](Reader::implying).
    installed: Option<(usize, u8)>,
    /// Members of internally tagged objects that came before their tag, to
    /// be read once the ones after it are: a stack, since an object whose tag
    /// is late can hold a member whose tag is late too. Empty, and never
    /// allocated, until a late tag is found. See
    /// [`read_internally_tagged`](Reader::read_internally_tagged).
    deferred: Vec<Deferred>,
    /// `fn() -> O` rather than `O`, so the reader's auto traits follow what it
    /// actually holds rather than a policy type it never contains.
    options: PhantomData<fn() -> O>,
}

impl<'de> Reader<'de> {
    /// Wrap a document, read under [`Standard`].
    ///
    /// This is the constructor to reach for. Hand-driving a reader is usually
    /// for walking a document's structure directly, where no setting applies;
    /// [`read_object`](Self::read_object) is the exception, and reads under
    /// [`Standard`] here like everything else.
    /// [`with_options`](Self::with_options) names a different policy, and is
    /// what the `_with` entry points use.
    ///
    /// ```
    /// use structio::beve::Reader;
    ///
    /// let r = Reader::new(&[]);
    /// assert_eq!(r.position(), 0);
    /// ```
    #[inline]
    pub fn new(data: &'de [u8]) -> Self {
        Self::with_options(data)
    }
}

impl<'de, O: Options> Reader<'de, O> {
    /// Wrap a document, read under the policy `O`.
    ///
    /// The policy is named once here and inferred everywhere after. A
    /// defaulted type parameter fills in a *type*; it does not tell inference
    /// what an associated function's `Self` is, which is why the default is
    /// reached through [`new`](Self::new) rather than by leaving `O` off.
    #[inline]
    pub fn with_options(data: &'de [u8]) -> Self {
        Reader {
            data,
            pos: 0,
            depth: 0,
            error_key: None,
            deferred: Vec::new(),
            implied: None,
            installed: None,
            options: PhantomData,
        }
    }

    /// A reader over one value whose header is not among its bytes.
    ///
    /// A typed array's elements carry no header of their own, so a span cut out
    /// of one is not something [`Reader::new`] could read. Installing the
    /// header the array implied makes it one, which is what lets
    /// [`beve::Documents`](crate::beve::Documents) hand a typed array's
    /// elements to the same [`Read`] impls as everything else. See the module
    /// docs on implied headers.
    #[inline]
    pub(crate) fn with_implied(data: &'de [u8], implied: u8) -> Self {
        Reader {
            data,
            pos: 0,
            depth: 0,
            error_key: None,
            deferred: Vec::new(),
            implied: Some(implied),
            installed: Some((0, implied)),
            options: PhantomData,
        }
    }

    /// Byte offset of the cursor, which is where an error is reported from.
    #[inline]
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Bytes not yet read. Every element of an array and every member of an
    /// object costs at least one of them, so this bounds how many a count off
    /// the wire can honestly claim.
    #[inline]
    pub(crate) fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    /// A count off the wire, clipped to what the input could hold.
    #[inline]
    fn honest(&self, n: usize) -> usize {
        n.min(self.remaining())
    }

    /// The key set for the failure being returned, if there is one.
    ///
    /// [`json::Parser::error_key`](crate::json::Parser::error_key)'s
    /// counterpart, and worth more here: a byte offset into a binary document
    /// is not something a person can read a document against, so for BEVE the
    /// key is often the whole of the diagnostic.
    #[inline]
    pub fn error_key(&self) -> Option<&'static str> {
        self.error_key
    }

    /// Name the key the failure about to be returned is about.
    ///
    /// See [`json::Parser::set_error_key`](crate::json::Parser::set_error_key),
    /// which this mirrors, including the rule about setting it after any
    /// [`rewind`](Self::rewind) and on the branch that is returning `Err`.
    #[inline]
    pub fn set_error_key(&mut self, key: &'static str) {
        self.error_key = Some(key);
    }

    /// Move the cursor back to a position it has already passed.
    ///
    /// The companion of [`position`](Self::position), and what a hand-written
    /// [`Read`] impl needs to report a failure against something it walked
    /// past. An [`Error`](crate::Error) carries no message, only a code and
    /// the offset the cursor stopped at, so pointing at the right byte is the
    /// whole of a good diagnostic: a reader that discovers at the end of an
    /// object that a member never arrived wants to name the object, not what
    /// follows it. That is exactly what [`read_object`](Self::read_object)
    /// does for [`Options::ERROR_ON_MISSING_KEYS`],
    /// and what [`Matrix`](crate::Matrix) does by hand.
    ///
    /// The cursor never moves forward: a position ahead of it leaves it where
    /// it is. Winding forward would step over bytes without reading them,
    /// which is not something a caller could mean by "rewind".
    ///
    /// ```
    /// use structio::beve::Reader;
    ///
    /// let doc = structio::to_beve(&vec![1u8, 2, 3]);
    /// let mut r = Reader::new(&doc);
    /// let start = r.position();
    /// r.skip_value().unwrap();
    /// assert_eq!(r.position(), doc.len());
    ///
    /// r.rewind(start);
    /// assert_eq!(r.position(), start);
    ///
    /// // Forward is not a rewind, so nothing happens.
    /// r.rewind(doc.len());
    /// assert_eq!(r.position(), start);
    /// ```
    /// Any key [`set_error_key`](Self::set_error_key) left is dropped, for
    /// the reason [`json::Parser::rewind`](crate::json::Parser::rewind) gives.
    ///
    /// Besides the cursor, the one piece of state wound back is the header of
    /// an element of a typed array, which a [`seek`](Self::seek) onto the
    /// element or a read of the array installs because the element's bytes
    /// hold none. A header is installed only while the cursor stands on its
    /// element, so winding back onto that element puts it back and winding
    /// anywhere else takes it away; a forward `to` leaves it as it leaves the
    /// cursor. The first is what lets a reader that speculates on an element,
    /// trying a number and then a string, wind back and retry as freely as one
    /// that speculates on a whole value: the try that took the header would
    /// otherwise leave the next one reading the element's first byte as its
    /// header. The second is what lets a reader that sought onto an element
    /// wind back to read something else, which would otherwise take the
    /// element's header as its own.
    ///
    /// Nothing else needs winding back, for the reason `json::Parser::rewind`
    /// gives.
    #[inline]
    pub fn rewind(&mut self, to: usize) {
        self.error_key = None;
        // Returning here is also what keeps `pos <= data.len()`, which every
        // bounds test in here is written against.
        if to > self.pos {
            return;
        }
        self.pos = to;
        self.installed = self.installed.filter(|&(at, _)| at == to);
        self.implied = self.installed.map(|(_, h)| h);
    }

    /// Confirm the document ended where the value did.
    pub fn finish(&mut self) -> PResult<()> {
        if self.pos == self.data.len() {
            Ok(())
        } else {
            Err(ErrorCode::TrailingContent)
        }
    }

    /// Read one value into `value`.
    #[inline]
    pub fn read<T: Read<'de>>(&mut self, value: &mut T) -> PResult<()> {
        value.read(self)
    }

    // -----------------------------------------------------------------------
    // Primitives
    // -----------------------------------------------------------------------

    /// The header of the value at the cursor, without consuming it.
    #[inline(always)]
    pub(crate) fn peek(&self) -> Option<u8> {
        match self.implied {
            Some(h) => Some(h),
            None => self.data.get(self.pos).copied(),
        }
    }

    /// Take the header of the value at the cursor.
    ///
    /// Inside a typed array this yields the installed element header and moves
    /// nothing; everywhere else it consumes a byte.
    #[inline(always)]
    pub(crate) fn head(&mut self) -> PResult<u8> {
        if let Some(h) = self.implied.take() {
            return Ok(h);
        }
        let &b = self.data.get(self.pos).ok_or(ErrorCode::UnexpectedEnd)?;
        self.pos += 1;
        Ok(b)
    }

    /// Take the next `n` bytes.
    #[inline]
    pub fn take(&mut self, n: usize) -> PResult<&'de [u8]> {
        let data = self.data;
        let end = self.pos.checked_add(n).ok_or(ErrorCode::UnexpectedEnd)?;
        if end > data.len() {
            return Err(ErrorCode::UnexpectedEnd);
        }
        let out = &data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    /// Skip the next `n` bytes.
    #[inline]
    fn drop_bytes(&mut self, n: usize) -> PResult<()> {
        self.take(n).map(|_| ())
    }

    /// Read a compressed size.
    #[inline]
    pub fn size(&mut self) -> PResult<u64> {
        decode_size(self.data, &mut self.pos)
    }

    /// Read a compressed size as a count of things.
    ///
    /// A count wider than the address space cannot describe anything in this
    /// buffer, so it is an end-of-input rather than its own error.
    #[inline]
    pub(crate) fn count(&mut self) -> PResult<usize> {
        usize::try_from(self.size()?).map_err(|_| ErrorCode::UnexpectedEnd)
    }

    /// Count one more level of nesting, or refuse it past [`MAX_DEPTH`].
    ///
    /// A refusal leaves the count where it was, for the reason
    /// [`nested`](Self::nested) gives.
    #[inline(always)]
    pub(crate) fn enter(&mut self) -> PResult<()> {
        if self.depth >= MAX_DEPTH {
            return Err(ErrorCode::ExceededMaxDepth);
        }
        self.depth += 1;
        Ok(())
    }

    /// Run `body` one level deeper, and come back up however it exits.
    ///
    /// The depth count is state that outlives a failed read, which the cursor
    /// is not: [`rewind`](Self::rewind) puts the cursor back, but a level
    /// entered and never left would stay counted, so a reader that speculates
    /// and winds back would lose a level per failure and, some hundreds of
    /// failures on, have ordinary input refused as too deep. So a failure
    /// releases what it entered on the way out, and every container walk goes
    /// through here rather than pairing `enter` and `leave` around `?`s that
    /// would skip the `leave`.
    #[inline(always)]
    pub(crate) fn nested<R>(&mut self, body: impl FnOnce(&mut Self) -> PResult<R>) -> PResult<R> {
        self.enter()?;
        let result = body(self);
        self.leave();
        result
    }

    /// Whether a container opened here fits under [`MAX_DEPTH`]: the test
    /// [`enter`](Self::enter) makes, without the count.
    ///
    /// For the reads that take a typed array whole, which are a copy, a borrow
    /// and a byte slice. A typed array costs a level however it is read, and
    /// nothing in one recurses, so the level would be entered and left with
    /// only a payload taken in between, and this test is the whole of the
    /// charge. It leaves no depth to restore on any path out. Taken free, a
    /// block would let reading accept a document one level deeper than
    /// validating, transcoding or framing does.
    #[inline(always)]
    fn can_enter(&self) -> bool {
        self.depth < MAX_DEPTH
    }

    /// Read the value at the cursor with `h` installed as its header, and take
    /// the header back however the read exits.
    ///
    /// An installed header is state beside the cursor, like the depth count,
    /// and it outlives a failed read the same way. A read that refuses a
    /// header it only looked at, as a hand-written impl that refuses whatever
    /// [`try_null`](Self::try_null) declines does, leaves it installed, and
    /// whatever its caller reads next without a [`rewind`](Self::rewind) takes
    /// it as its own header and reads the bytes after it as a value of that
    /// type. So every walk that installs one for a read of its own goes through
    /// here, as every container walk goes through [`nested`](Self::nested),
    /// rather than clearing it after a loop that a `?` can leave early. The
    /// one install that outlasts its call is [`seek`](Self::seek)'s, which
    /// hands the element to its caller: the caller's read takes the header,
    /// and a rewind anywhere but onto the element takes it away.
    ///
    /// Where the value begins is recorded beside the header, for as long as
    /// the read lasts, which is what lets `rewind` put the header back when a
    /// read that took it winds back to try again. The record `body` found is
    /// restored on the way out, since a complex number's components are read
    /// inside the read of the element that holds them.
    #[inline(always)]
    fn implying<R>(&mut self, h: u8, body: impl FnOnce(&mut Self) -> PResult<R>) -> PResult<R> {
        let outer = self.installed;
        self.install(h);
        let result = body(self);
        self.implied = None;
        self.installed = outer;
        result
    }

    /// Install `h` as the header of the value at the cursor, which carries
    /// none of its own.
    #[inline(always)]
    fn install(&mut self, h: u8) {
        self.implied = Some(h);
        self.installed = Some((self.pos, h));
    }

    /// Leave a container [`enter`](Self::enter) counted.
    ///
    /// The two are balanced by the caller, on every exit, and the public
    /// `read_object_rest` / `finish_internally_tagged` pair takes its `enter`
    /// from whoever opened the object. A hand-written impl that calls one of
    /// those without having entered wraps the depth, and what that costs
    /// depends on how often it happens: once, the next `enter` wraps it back
    /// and the parse allows one extra level; repeatedly, the limit refuses
    /// input it should have taken. It stops counting altogether only when a
    /// stray `leave` cancels an `enter` at every level of a recursion, and
    /// then a hostile document has no depth limit at all. A debug build says
    /// so rather than leaving any of that to be discovered.
    #[inline(always)]
    pub(crate) fn leave(&mut self) {
        debug_assert!(
            self.depth > 0,
            "structio: `leave` without a matching `enter`, which would disable the nesting limit"
        );
        self.depth -= 1;
    }

    /// Consume an aligned typed array's preamble, leaving the cursor on the
    /// payload, and report its element header and count.
    ///
    /// The form is `HEADER | NUMERIC_HEADER | SIZE | PADDING_LENGTH | PADDING |
    /// DATA`: the padding exists so a reader can point straight at `DATA`, and
    /// its length is stated rather than derived, so stepping over it needs no
    /// knowledge of where the buffer started. The outer header is already
    /// consumed when this is called.
    ///
    /// Four paths meet this form, and they have to agree on it, so they share
    /// the walk and differ only in what they do with the payload. What they
    /// must agree on includes which inner headers exist at all: the aligned
    /// form wraps a *numeric typed array*, and the width comes from bits the
    /// type field does not touch, so a path that omitted this check would
    /// silently accept an inner header of some other kind and compute an
    /// extent from it. That is how a validator comes to accept a document a
    /// reader rejects. Anything narrower, such as the one-byte elements a
    /// borrowed `&[u8]` needs, is still the caller's to require.
    ///
    /// A padding length of the element's width or more is
    /// [`InvalidPadding`](ErrorCode::InvalidPadding), just past the length
    /// byte; see [`aligned_padding`].
    fn aligned_head(&mut self) -> PResult<(u8, usize)> {
        let inner = self.head()?;
        if header::ty(inner) != header::TY_TYPED_ARRAY || header::sub(inner) == header::CAT_OTHER {
            return Err(ErrorCode::InvalidHeader);
        }
        // Refused on the header, before the count, as `typed_head` refuses an
        // unaligned array of the same element type.
        let width = fixed_width(inner)?;
        let n = self.count()?;
        let pad = self.take(1)?[0];
        aligned_padding(pad, width)?;
        self.drop_bytes(usize::from(pad))?;
        Ok((inner, n))
    }

    /// Confirm `n` more bytes are in the buffer, without consuming them.
    ///
    /// A count comes off the wire and need not describe the bytes that follow
    /// it, so a payload's extent is checked before anything walks it: a bogus
    /// count must not drag a caller through millions of doomed iterations, and
    /// an offset into a payload must not land outside the buffer.
    #[inline]
    fn have(&self, n: usize) -> PResult<()> {
        match self.pos.checked_add(n) {
            Some(end) if end <= self.data.len() => Ok(()),
            _ => Err(ErrorCode::UnexpectedEnd),
        }
    }

    /// Consume a typed array's preamble and report which form it is, leaving
    /// the cursor on the payload.
    ///
    /// Reading a typed array, stepping over one, and indexing into one all
    /// begin with this same decision and differ only in what they then do with
    /// the payload. Deciding it once is what keeps them from drifting apart
    /// about where a value ends.
    ///
    /// A packed-boolean payload is also confirmed present and the padding in
    /// its last byte checked, here rather than in each walk so that they all
    /// refuse the same arrays and no per-element loop pays for it. Non-zero
    /// padding is [`InvalidPadding`](ErrorCode::InvalidPadding), just past
    /// that byte.
    pub(crate) fn typed_head(&mut self, h: u8) -> PResult<Typed> {
        let form = self.typed_preamble(h)?;
        if let Typed::Bools(n) = form {
            let bytes = n.div_ceil(8);
            self.have(bytes)?;
            if let Some(last) = bytes.checked_sub(1)
                && let Err(e) = bool_padding(n, self.data[self.pos + last])
            {
                self.pos += bytes;
                return Err(e);
            }
        }
        Ok(form)
    }

    /// [`typed_head`](Self::typed_head) without the packed-boolean payload.
    ///
    /// For the stream framer's array mode alone, which hands a top-level
    /// array's elements out as their bytes arrive rather than holding the
    /// whole payload, and so checks the padding with [`bool_padding`] when the
    /// last element's byte comes due.
    pub(crate) fn typed_preamble(&mut self, h: u8) -> PResult<Typed> {
        match header::sub(h) {
            header::CAT_OTHER => match header::count(h) {
                header::OTHER_BOOL => Ok(Typed::Bools(self.count()?)),
                header::OTHER_STRING => Ok(Typed::Strings(self.count()?)),
                // The aligned form states its element type in a second header
                // and pads the payload so a reader can point at it directly.
                // Past the preamble it is an ordinary fixed-width block.
                header::OTHER_ALIGNED => {
                    let (inner, n) = self.aligned_head()?;
                    Ok(Typed::Fixed(inner, n))
                }
                _ => Err(ErrorCode::InvalidHeader),
            },
            // An element type the format does not define is known from the
            // header, so it is refused before the count is taken, as a lone
            // number of that type is, and a borrowed `&[u8]`, which reads no
            // count before deciding, stops in the same place.
            _ => {
                fixed_width(h)?;
                Ok(Typed::Fixed(h, self.count()?))
            }
        }
    }

    /// Consume a complex value's class header and, for the run form, its
    /// count, leaving the cursor on the payload.
    ///
    /// [`header::COMPLEX`] is already consumed. Reports the class header, the
    /// width of one component, and how many pairs follow, `None` being the
    /// lone form.
    ///
    /// Shared for the same reason [`Self::typed_head`] is: the two forms differ
    /// by a size in front of the payload, so a walk that decided this for
    /// itself would eventually step over a different extent than the others.
    pub(crate) fn complex_head(&mut self) -> PResult<(u8, usize, Option<usize>)> {
        let class = self.head()?;
        let width =
            byte_width(header::sub(class), header::count(class)).ok_or(ErrorCode::InvalidHeader)?;
        // The low three bits are three bits wide only so the class and byte
        // count land where a number header puts them. Two values are defined
        // and the other six carry no meaning; guessing would make the extent
        // of the value depend on them, so they are refused.
        let pairs = match class & 0b111 {
            header::COMPLEX_ONE => None,
            header::COMPLEX_MANY => Some(self.count()?),
            _ => return Err(ErrorCode::InvalidHeader),
        };
        Ok((class, width, pairs))
    }

    /// Decide how the value at the cursor holds one complex number.
    ///
    /// `Some(elem)` means a complex extension whose preamble is now consumed:
    /// the cursor is on the real part and `elem` is the number header both
    /// components carry, which is what hands them to the ordinary scalar
    /// readers and so gives a complex value the same width leniency every
    /// other number gets. `None` means an array, consumed nothing, and asks
    /// the caller to read two elements out of it, which is the form a producer
    /// without the extension writes and the form the JSON side always uses.
    pub(crate) fn complex_form(&mut self) -> PResult<Option<u8>> {
        // Inside a complex array the header was installed rather than read,
        // and it is the synthetic one. Its class and width sit in the fields a
        // number header uses, so the components' header falls out of it by the
        // same swap of the type bits a typed array's element header takes.
        if let Some(h) = self.implied.take() {
            if header::ty(h) != header::TY_UNDEFINED {
                return Err(ErrorCode::ExpectedComplex);
            }
            return Ok(Some(header::element_of(h)));
        }
        match self.peek() {
            Some(header::COMPLEX) => {
                self.head()?;
                let (class, _, pairs) = self.complex_head()?;
                if pairs.is_some() {
                    // A run of complex numbers is a sequence, not one value.
                    return Err(ErrorCode::ExpectedComplex);
                }
                Ok(Some(header::element_of(class)))
            }
            Some(h)
                if matches!(
                    header::ty(h),
                    header::TY_GENERIC_ARRAY | header::TY_TYPED_ARRAY
                ) =>
            {
                Ok(None)
            }
            // Taken before it is refused, as an installed header is above, so
            // the cursor stops just past it where every other type mismatch
            // leaves it.
            Some(_) => {
                self.head()?;
                Err(ErrorCode::ExpectedComplex)
            }
            None => Err(ErrorCode::UnexpectedEnd),
        }
    }

    /// Read the two components of a complex value whose preamble is consumed,
    /// each under the header [`Self::complex_form`] reported.
    pub(crate) fn complex_pair<T: Read<'de>>(
        &mut self,
        elem: u8,
        re: &mut T,
        im: &mut T,
    ) -> PResult<()> {
        self.implying(elem, |r| re.read(r))?;
        self.implying(elem, |r| im.read(r))
    }

    // -----------------------------------------------------------------------
    // Scalars
    // -----------------------------------------------------------------------

    #[inline]
    pub fn read_bool(&mut self) -> PResult<bool> {
        match self.head()? {
            header::TRUE => Ok(true),
            header::FALSE => Ok(false),
            // A null is a value and not a boolean. Anything else of the null
            // and boolean type is no value at all, as every other walk says.
            header::NULL => Err(ErrorCode::ExpectedBool),
            h if header::ty(h) == header::TY_NULL_BOOL => Err(ErrorCode::InvalidHeader),
            _ => Err(ErrorCode::ExpectedBool),
        }
    }

    /// Consume a `null` if that is what is here, and report whether it was.
    #[inline]
    pub fn try_null(&mut self) -> PResult<bool> {
        match self.peek() {
            Some(header::NULL) => {
                self.head()?;
                Ok(true)
            }
            Some(_) => Ok(false),
            None => Err(ErrorCode::UnexpectedEnd),
        }
    }

    /// Read an integer of any stored width and signedness.
    fn read_int(&mut self) -> PResult<Int> {
        let h = self.head()?;
        if header::ty(h) != header::TY_NUMBER {
            return Err(ErrorCode::ExpectedNumber);
        }
        let cat = header::sub(h);
        // A width the format does not define makes the header no number at
        // all, of either kind, so that is settled first, as `number_body`
        // settles it and as every walk that is not after an integer does.
        let width = byte_width(cat, header::count(h)).ok_or(ErrorCode::InvalidHeader)?;
        // The header alone says a float is no integer, so it is refused before
        // the payload is taken. Taking it first would leave the cursor, and
        // the offset the entry point attaches, past the value rather than on
        // it where every other type mismatch leaves them, and a caller that
        // tries another reading without rewinding would start on the next
        // value.
        if cat == header::CAT_FLOAT {
            return Err(ErrorCode::ExpectedInteger);
        }
        let bytes = self.take(width)?;
        match cat {
            header::CAT_UNSIGNED => Ok(Int::Unsigned(le_u128(bytes))),
            header::CAT_SIGNED => Ok(Int::Signed(sign_extend(le_u128(bytes), width))),
            _ => Err(ErrorCode::ExpectedInteger),
        }
    }

    #[inline]
    pub fn read_u64(&mut self) -> PResult<u64> {
        match self.read_int()? {
            Int::Unsigned(v) => u64::try_from(v).map_err(|_| ErrorCode::NumberOutOfRange),
            Int::Signed(v) => u64::try_from(v).map_err(|_| ErrorCode::NumberOutOfRange),
        }
    }

    #[inline]
    pub fn read_i64(&mut self) -> PResult<i64> {
        match self.read_int()? {
            Int::Unsigned(v) => i64::try_from(v).map_err(|_| ErrorCode::NumberOutOfRange),
            Int::Signed(v) => i64::try_from(v).map_err(|_| ErrorCode::NumberOutOfRange),
        }
    }

    #[inline]
    pub fn read_u128(&mut self) -> PResult<u128> {
        match self.read_int()? {
            Int::Unsigned(v) => Ok(v),
            Int::Signed(v) => u128::try_from(v).map_err(|_| ErrorCode::NumberOutOfRange),
        }
    }

    #[inline]
    pub fn read_i128(&mut self) -> PResult<i128> {
        match self.read_int()? {
            Int::Unsigned(v) => i128::try_from(v).map_err(|_| ErrorCode::NumberOutOfRange),
            Int::Signed(v) => Ok(v),
        }
    }

    /// Read any number as an `f64`.
    ///
    /// Integers convert, which is what makes a `f64` field able to read a
    /// document whose producer happened to have an integral value in it, the
    /// same way `1` parses into an `f64` from JSON.
    pub fn read_f64(&mut self) -> PResult<f64> {
        let (cat, code, bytes) = self.number_body()?;
        widen(cat, code, bytes)
    }

    /// Read any number as an `f32`.
    ///
    /// A stored `f32` is taken bit for bit rather than widened and narrowed
    /// back. The round trip through `f64` is exact for every finite value, but
    /// it does not carry a NaN's payload, and the bulk path takes the same
    /// bytes without touching them: a `Vec<f32>` and an `f32` field must not
    /// disagree about what came off the wire.
    pub fn read_f32(&mut self) -> PResult<f32> {
        let (cat, code, bytes) = self.number_body()?;
        if cat == header::CAT_FLOAT && code == 2 {
            return Ok(f32::from_le_bytes(bytes.try_into().expect("four bytes")));
        }
        widen(cat, code, bytes).map(|v| v as f32)
    }

    /// Consume a number header and its payload, reporting the category, the
    /// width code, and the bytes.
    ///
    /// Shared so that the two float readers cannot come to disagree about
    /// which headers are numbers or how wide each one is.
    #[inline]
    fn number_body(&mut self) -> PResult<(u8, u8, &'de [u8])> {
        let h = self.head()?;
        if header::ty(h) != header::TY_NUMBER {
            return Err(ErrorCode::ExpectedNumber);
        }
        let cat = header::sub(h);
        let code = header::count(h);
        // A 128-bit float is refused here too, on the header, for the reason
        // `read_int` refuses a float before taking the payload.
        let width = header::decodable_width(cat, code)?;
        Ok((cat, code, self.take(width)?))
    }

    /// Read a string, borrowed straight out of the input.
    ///
    /// BEVE strings are stored verbatim, so unlike JSON there is no escaped
    /// form that would have to be rebuilt: every string borrows.
    #[inline]
    pub fn read_str(&mut self) -> PResult<&'de str> {
        let h = self.head()?;
        if header::ty(h) != header::TY_STRING {
            return Err(ErrorCode::ExpectedString);
        }
        bare_header(h)?;
        self.str_body()
    }

    /// Read a string into an existing `String`, keeping its allocation.
    #[inline]
    pub fn read_string_into(&mut self, out: &mut String) -> PResult<()> {
        let s = self.read_str()?;
        out.clear();
        out.push_str(s);
        Ok(())
    }

    /// The `SIZE | DATA` half of a string, with the header already dealt with.
    #[inline]
    pub(crate) fn str_body(&mut self) -> PResult<&'de str> {
        let n = self.count()?;
        self.str_text(n)
    }

    /// The `DATA` half alone, for a caller that read the size itself because
    /// it wanted the position the text starts at.
    #[inline]
    pub(crate) fn str_text(&mut self, n: usize) -> PResult<&'de str> {
        let bytes = self.take(n)?;
        core::str::from_utf8(bytes).map_err(|_| ErrorCode::InvalidUtf8)
    }

    /// Borrow a byte array straight out of the input.
    ///
    /// Accepts a typed array of one-byte elements, of either signedness, and
    /// the aligned form of the same. A wider element type is not a run of
    /// bytes and is reported rather than reinterpreted. The array costs a
    /// nesting level, as it does read any other way.
    pub fn read_bytes(&mut self) -> PResult<&'de [u8]> {
        let h = self.head()?;
        if header::ty(h) != header::TY_TYPED_ARRAY {
            return Err(ErrorCode::ExpectedArray);
        }
        // Charged as `read_seq` charges a typed array, and refused at the
        // point it refuses one: before the element type is looked at.
        if !self.can_enter() {
            return Err(ErrorCode::ExceededMaxDepth);
        }
        if header::sub(h) != header::CAT_OTHER {
            byte_elements(h)?;
            let n = self.count()?;
            return self.take(n);
        }
        match header::count(h) {
            // The aligned form states its element type in a second header, and
            // pads the payload so a reader can point at it directly.
            header::OTHER_ALIGNED => {
                let (inner, n) = self.aligned_head()?;
                byte_elements(inner)?;
                self.take(n)
            }
            header::OTHER_BOOL | header::OTHER_STRING => Err(ErrorCode::ExpectedBytes),
            // No form at all, as `typed_head` says of it.
            _ => Err(ErrorCode::InvalidHeader),
        }
    }

    // -----------------------------------------------------------------------
    // Objects
    // -----------------------------------------------------------------------

    /// Read a BEVE object into a type declared with `object!`.
    ///
    /// One iteration per member: take the key, hash it to a candidate field,
    /// let the generated dispatch confirm it and read the value, and skip the
    /// member whole if no field claimed it.
    pub fn read_object<T: ReadObject<'de>>(&mut self, value: &mut T) -> PResult<()> {
        // Where the object begins, so a member it never carried can be
        // reported against the object rather than against whatever follows it.
        // Dead, and gone, under a policy that requires nothing.
        let open = self.pos;
        let h = self.head()?;
        if header::ty(h) != header::TY_OBJECT {
            return Err(ErrorCode::ExpectedObject);
        }
        // A key width the format does not define makes the header no object at
        // all, which is settled before what its keys are, as `read_int`
        // settles a number's width before its kind.
        key_width(h)?;
        if header::sub(h) != header::CAT_FLOAT {
            // Categories 1 and 2 are integer keys, which no `object!` struct
            // has: its keys are names.
            return Err(ErrorCode::UnsupportedKeyType);
        }
        let members = self.count()?;
        self.enter()?;
        self.read_object_rest(value, members, open)
    }

    /// Read `remaining` members into a type declared with `object!`, the
    /// object's header and any members before them already consumed.
    ///
    /// What [`Self::read_internally_tagged`] leaves behind: the tag has been
    /// taken, and the rest of the object is the variant's payload. It is also
    /// [`Self::read_object`]'s own body, an object with nothing taken from it
    /// yet being the case where `remaining` is all of it.
    ///
    /// `open` is the offset of the object's header byte, carried in because a
    /// [`MissingKey`](ErrorCode::MissingKey) is reported against the object
    /// rather than against what follows it, and this is called once the cursor
    /// is past it. The `enter` is the caller's, and is balanced here, whether
    /// or not the members read.
    pub fn read_object_rest<T: ReadObject<'de>>(
        &mut self,
        value: &mut T,
        remaining: usize,
        open: usize,
    ) -> PResult<()> {
        let seen = self.rest_members::<T>(value, remaining);
        self.leave();
        let seen = seen?;
        let mask = Fields::<O, T>::MASK;
        if seen & mask != mask {
            // Back to the object's header: the cursor is past the object by
            // now, and what is incomplete is the object, not what follows it.
            // The offset can therefore only name the object, so the key of the
            // member it lacks is carried alongside it.
            self.pos = open;
            self.error_key = Fields::<O, T>::missing(seen);
            return Err(ErrorCode::MissingKey);
        }
        Ok(())
    }

    /// [`read_object_rest`](Self::read_object_rest) short of its `leave`, so
    /// that a member failing to read cannot skip it.
    #[inline(always)]
    fn rest_members<T: ReadObject<'de>>(
        &mut self,
        value: &mut T,
        remaining: usize,
    ) -> PResult<u64> {
        // One bit per field filled, compared once the object ends against the
        // fields that had to be there. Never written, and so never read, unless
        // the policy or the type asks for one.
        let mut seen = 0u64;
        for _ in 0..remaining {
            self.object_member::<T>(value, &mut seen)?;
        }
        if let Some(run) = self.take_deferred() {
            let resume = self.pos;
            self.pos = run.start;
            for _ in 0..run.count {
                self.object_member::<T>(value, &mut seen)?;
            }
            self.pos = resume;
        }
        Ok(seen)
    }

    /// One member, the cursor sitting on its key: look the key up, let the
    /// generated dispatch confirm it and read the value, and note the field
    /// in `seen` if it was one.
    #[inline]
    fn object_member<T: ReadObject<'de>>(&mut self, value: &mut T, seen: &mut u64) -> PResult<()> {
        let map = T::MAP;
        let keys = map.n as usize;
        let n = self.count()?;
        // Where the key's bytes begin, so a refusal can point at them rather
        // than at the value they introduced. Dead, and gone, under a policy
        // that cannot refuse.
        let at = self.pos;
        let key = self.take(n)?;
        // The hash indexes every key, aliases included; the dispatch has an
        // arm per field, so an alias goes back to the field it fills first.
        let mut index = map.lookup_sized(T::KEYS, key);
        let matched = index < keys && {
            index = resolve_key::<T>(index);
            T::read_field(value, index, key, self)?
        };
        if Fields::<O, T>::TRACK && matched {
            *seen |= Fields::<O, T>::seen(index);
        }
        if !matched {
            if O::ERROR_ON_UNKNOWN_KEYS {
                self.pos = at;
                return Err(ErrorCode::UnknownKey);
            }
            self.skip_value()?;
        }
        Ok(())
    }

    /// The deferred run, if it is this object's to read.
    ///
    /// The innermost run is the top of the stack, and only the object that
    /// pushed it is at its depth when the payload ends, so a depth match is
    /// ownership.
    #[inline(always)]
    fn take_deferred(&mut self) -> Option<Deferred> {
        match self.deferred.last() {
            Some(run) if run.depth == self.depth => self.deferred.pop(),
            _ => None,
        }
    }

    /// Consume `remaining` members of an object with no fields to fill: the
    /// form an internally tagged variant carrying nothing takes.
    ///
    /// The tag was the whole value, so anything after it is an unknown member
    /// and meets the policy that governs one. The `enter` is the caller's, and
    /// is balanced here, whether or not the members were acceptable.
    pub fn finish_internally_tagged(&mut self, remaining: usize) -> PResult<()> {
        let result = self.unknown_members(remaining);
        self.leave();
        result
    }

    /// [`finish_internally_tagged`](Self::finish_internally_tagged) short of
    /// its `leave`, for [`read_object_rest`](Self::read_object_rest)'s reason.
    #[inline(always)]
    fn unknown_members(&mut self, remaining: usize) -> PResult<()> {
        for _ in 0..remaining {
            self.unknown_member()?;
        }
        if let Some(run) = self.take_deferred() {
            let resume = self.pos;
            self.pos = run.start;
            for _ in 0..run.count {
                self.unknown_member()?;
            }
            self.pos = resume;
        }
        Ok(())
    }

    /// Step over a member no field claims, or refuse it, as the policy says.
    fn unknown_member(&mut self) -> PResult<()> {
        let n = self.count()?;
        let at = self.pos;
        self.take(n)?;
        if O::ERROR_ON_UNKNOWN_KEYS {
            self.pos = at;
            return Err(ErrorCode::UnknownKey);
        }
        self.skip_value()
    }

    /// Read a BEVE enum into a type declared with `unit_enum!` or
    /// `tagged_enum!`.
    ///
    /// Two forms, told apart by the header. A string is a variant carrying
    /// nothing; an object of exactly one member is one carrying a value, keyed
    /// by the name. Either way the name is hashed to a candidate variant, and
    /// the generated dispatch confirms it.
    ///
    /// A name no variant claims is an
    /// [`ErrorCode::UnknownVariant`] under every policy, including
    /// [`SkipUnknown`](crate::SkipUnknown), for the reason
    /// [`json::Parser::read_enum`](crate::json::Parser::read_enum) gives.
    pub fn read_enum<T: ReadEnum<'de>>(&mut self, value: &mut T) -> PResult<()> {
        // Where the value begins, so a name nothing claims is reported against
        // the value rather than against whatever followed it.
        let open = self.pos;
        let h = self.head()?;
        match header::ty(h) {
            header::TY_STRING => {
                bare_header(h)?;
                let name = self.str_body()?.as_bytes();
                // The hash only proposes a variant; `read_name` confirms the
                // name itself and may still decline.
                let index = T::MAP.lookup_sized(T::VARIANTS, name);
                if index >= T::MAP.n as usize
                    || !T::read_name(value, resolve_variant::<T>(index), name)?
                {
                    self.pos = open;
                    return Err(ErrorCode::UnknownVariant);
                }
                Ok(())
            }
            header::TY_OBJECT => {
                // Width before kind, as `read_object` has it.
                key_width(h)?;
                if header::sub(h) != header::CAT_FLOAT {
                    // Integer keys, which no enum has: its variants are names.
                    return Err(ErrorCode::UnsupportedKeyType);
                }
                // The tag is the object's whole content. Any other count names
                // no variant of anything.
                if self.count()? != 1 {
                    self.pos = open;
                    return Err(ErrorCode::ExpectedVariant);
                }
                self.nested(|r| {
                    let n = r.count()?;
                    // Where the name's bytes begin, so a refusal points at
                    // them rather than at the value they introduced.
                    let at = r.pos;
                    let name = r.take(n)?;
                    let index = T::MAP.lookup_sized(T::VARIANTS, name);
                    if index >= T::MAP.n as usize
                        || !T::read_payload(value, resolve_variant::<T>(index), name, r)?
                    {
                        r.pos = at;
                        return Err(ErrorCode::UnknownVariant);
                    }
                    Ok(())
                })
            }
            _ => {
                self.pos = open;
                Err(ErrorCode::ExpectedVariant)
            }
        }
    }

    /// Read a BEVE object into a type declared with a tag clause:
    /// `tagged_enum!(.. as tag "..")`.
    ///
    /// The mirror of
    /// [`json::Parser::read_internally_tagged`](crate::json::Parser::read_internally_tagged),
    /// and it finds a late tag the same way: the members before it are
    /// stepped over, the members after it are read, and then the ones
    /// stepped over are read into the same value. Those members cost two
    /// walks, and a key present on both sides of the tag keeps its earlier
    /// value. A payload member that is itself tagged late stacks its own run
    /// on this one. An object with no tag at all is
    /// [`ErrorCode::ExpectedTag`], reported against its first key.
    ///
    /// A tag that names no variant is [`ErrorCode::UnknownVariant`] under
    /// every policy, for the reason [`Self::read_enum`] gives.
    pub fn read_internally_tagged<T: ReadInternallyTagged<'de>>(
        &mut self,
        value: &mut T,
    ) -> PResult<()> {
        // Where the object begins, so a payload missing a required member is
        // reported against the object, as it is for a struct.
        let open = self.pos;
        let h = self.head()?;
        if header::ty(h) != header::TY_OBJECT {
            return Err(ErrorCode::ExpectedObject);
        }
        // Width before kind, as `read_object` has it.
        key_width(h)?;
        if header::sub(h) != header::CAT_FLOAT {
            // Integer keys, which no enum has: its tag is a name.
            return Err(ErrorCode::UnsupportedKeyType);
        }
        let members = self.count()?;
        // No members at all: no tag, and so no variant.
        if members == 0 {
            self.pos = open;
            return Err(ErrorCode::ExpectedTag);
        }
        // What a failure puts back, for the reason
        // [`json::Parser::read_internally_tagged`](crate::json::Parser::read_internally_tagged)
        // gives: the level is left by whichever of `read_object_rest` and
        // `finish_internally_tagged` ends the generated arm, if a failure got
        // that far, and a late-tag run left on the stack would be taken by the
        // next object read at its depth.
        let depth = self.depth;
        let runs = self.deferred.len();
        let result = self.tagged_object::<T>(value, members, open);
        if result.is_err() {
            self.depth = depth;
            self.deferred.truncate(runs);
        }
        result
    }

    /// [`read_internally_tagged`](Self::read_internally_tagged) past the
    /// object's header and count, which `open` and `members` are.
    #[inline(always)]
    fn tagged_object<T: ReadInternallyTagged<'de>>(
        &mut self,
        value: &mut T,
        members: usize,
        open: usize,
    ) -> PResult<()> {
        self.enter()?;

        // Where the first member's key begins, which is what a tag that is not
        // here is reported against.
        let first = self.pos;
        let n = self.count()?;
        let mut after = members - 1;
        if self.take(n)? != T::TAG.as_bytes() {
            // The tag is somewhere later, or nowhere. Step over members until
            // it turns up; the ones stepped over are read after the payload's
            // own, and `after` is what is left past the tag.
            self.pos = first;
            let mut before = 0;
            loop {
                if before == members {
                    self.pos = first;
                    return Err(ErrorCode::ExpectedTag);
                }
                let n = self.count()?;
                if self.take(n)? == T::TAG.as_bytes() {
                    break;
                }
                self.skip_value()?;
                before += 1;
            }
            after = members - 1 - before;
            self.deferred.push(Deferred {
                start: first,
                count: before,
                depth: self.depth,
            });
        }
        // The tag's value names the variant, so it is a string or it is
        // nothing this can dispatch on.
        let at = self.pos;
        let vh = self.head()?;
        if header::ty(vh) != header::TY_STRING {
            self.pos = first;
            return Err(ErrorCode::ExpectedTag);
        }
        bare_header(vh)?;
        let name = self.str_body()?.as_bytes();

        // From here the generated arm owns the object's remaining members,
        // because only it knows the payload's type.
        let index = T::MAP.lookup_sized(T::VARIANTS, name);
        if index >= T::MAP.n as usize
            || !T::read_variant(value, resolve_variant::<T>(index), name, self, after, open)?
        {
            self.pos = at;
            return Err(ErrorCode::UnknownVariant);
        }
        Ok(())
    }

    /// Drive a BEVE object as a map, calling `entry` with each key.
    ///
    /// The key arrives already typed: BEVE stores integer keys as integers, so
    /// a `HashMap<u32, _>` round-trips without the stringification JSON forces.
    pub fn read_map<F>(&mut self, entry: F) -> PResult<()>
    where
        F: FnMut(&mut Self, Key<'de>) -> PResult<()>,
    {
        self.read_map_counted(|_| entry)
    }

    /// [`read_map`](Self::read_map), telling the caller where each key begins.
    ///
    /// The offset is the key's first byte: its raw UTF-8 for a string key,
    /// past the length prefix, and its little-endian bytes for an integer one.
    /// For a string key that is the byte [`ErrorCode::UnknownKey`] reports, so
    /// a reader refusing one here can hand the offset straight to [`Error`].
    /// An integer key has no such counterpart to agree with, `read_object`
    /// refusing an integer-keyed object outright as an
    /// [`UnsupportedKeyType`](crate::ErrorCode::UnsupportedKeyType).
    ///
    /// Take the name here rather than planning to recover it later: there is
    /// no BEVE [`Error::key_in`], for the reason given there.
    ///
    /// [`Error`]: crate::Error
    /// [`Error::key_in`]: crate::Error::key_in
    /// [`ErrorCode::UnknownKey`]: crate::ErrorCode::UnknownKey
    pub fn read_map_located<F>(&mut self, entry: F) -> PResult<()>
    where
        F: FnMut(&mut Self, Key<'de>, usize) -> PResult<()>,
    {
        self.drive_map(|_| entry)
    }

    /// [`read_map`](Self::read_map), telling the caller how many members to
    /// expect before the first one is read.
    ///
    /// `start` is called once with the member count clipped to the bytes
    /// left, since every member costs at least one, and returns the closure
    /// that reads each entry. It is for a [`cautious`] reservation, exactly
    /// as [`read_seq_counted`](Self::read_seq_counted)'s is.
    pub fn read_map_counted<S, F>(&mut self, start: S) -> PResult<()>
    where
        S: FnOnce(usize) -> F,
        F: FnMut(&mut Self, Key<'de>) -> PResult<()>,
    {
        self.drive_map(|n| {
            let mut entry = start(n);
            move |r: &mut Self, key, _| entry(r, key)
        })
    }

    /// The loop the three public forms share, in the widest shape: counted,
    /// and told where each key began. Private, no caller having wanted both.
    fn drive_map<S, F>(&mut self, start: S) -> PResult<()>
    where
        S: FnOnce(usize) -> F,
        F: FnMut(&mut Self, Key<'de>, usize) -> PResult<()>,
    {
        let h = self.head()?;
        if header::ty(h) != header::TY_OBJECT {
            return Err(ErrorCode::ExpectedObject);
        }
        let cat = header::sub(h);
        let width = key_width(h)?;
        let members = self.count()?;
        let mut entry = start(self.honest(members));
        self.nested(|r| {
            for _ in 0..members {
                // An integer key starts where the member does. A string key
                // does not: its length comes first, and the position wanted is
                // the text after it, which is where `object_member` winds back
                // to before refusing. So the string arm shadows this.
                let at = r.pos;
                let (key, at) = match cat {
                    header::CAT_FLOAT => {
                        let n = r.count()?;
                        let at = r.pos;
                        (Key::Str(r.str_text(n)?), at)
                    }
                    header::CAT_SIGNED => {
                        (Key::Signed(sign_extend(le_u128(r.take(width)?), width)), at)
                    }
                    _ => (Key::Unsigned(le_u128(r.take(width)?)), at),
                };
                entry(r, key, at)?;
            }
            Ok(())
        })
    }

    // -----------------------------------------------------------------------
    // Sequences
    // -----------------------------------------------------------------------

    /// Read a BEVE array into a type declared with `array!`.
    ///
    /// Position is the whole schema, so there is no key to hash and none to
    /// confirm: element `i` goes to field `i`, and the only thing to check is
    /// that the document held exactly as many as the struct has.
    pub fn read_array<T: ReadArray<'de>>(&mut self, value: &mut T) -> PResult<()> {
        let count = self.read_seq(|r, i| value.read_element(i, r))?;
        if count != T::LEN {
            return Err(ErrorCode::ArrayLengthMismatch);
        }
        Ok(())
    }

    /// Drive a sequence, calling `element` once per entry.
    ///
    /// Accepts a generic array or any typed array; `element` sees one value
    /// either way, because a typed array's element header is installed before
    /// each call. `element` receives the zero-based position so container
    /// implementations can reuse storage they already hold.
    ///
    /// # Element positions do not bound documents
    ///
    /// Installed, not present: that header is supplied by this reader and is
    /// not among the input's bytes, so a span cut between two
    /// [`position`](Self::position)s is not a value [`Reader::new`] could
    /// read. Out of a typed array it is headerless payload, and the first
    /// payload byte is then taken for a header; out of a packed boolean run
    /// it is empty, the cursor staying put until the run is done; out of a
    /// [complex array](header::COMPLEX), whose elements are bare pairs, it is
    /// headerless too. Only a generic array's elements are self-contained
    /// values in the input.
    ///
    /// A caller that wants each element as a document of its own should
    /// therefore not cut spans here.
    /// [`Documents::array`](crate::beve::Documents::array) over the same bytes
    /// hands out one element at a time with the header installed, takes a
    /// different type per call, and accepts every array shape:
    ///
    /// ```
    /// # use structio::beve::{Documents, to_vec};
    /// let bytes = to_vec(&vec![1.5f64, 2.5]); // a typed array
    /// let mut docs = Documents::array(&bytes[..]);
    /// let mut first = 0f64;
    /// docs.next_value_into(&mut first).unwrap()?;
    /// assert_eq!(first, 1.5);
    /// # Ok::<(), structio::StreamError>(())
    /// ```
    ///
    /// Cutting spans regardless means checking the header type first and
    /// taking only [`TY_GENERIC_ARRAY`](header::TY_GENERIC_ARRAY); a walk
    /// alone cannot tell the shapes apart, because the closure signature and
    /// [`position`](Self::position) are the same either way.
    pub fn read_seq<F>(&mut self, element: F) -> PResult<usize>
    where
        F: FnMut(&mut Self, usize) -> PResult<()>,
    {
        self.read_seq_counted(|_| element)
    }

    /// [`read_seq`](Self::read_seq), telling the caller how many elements to
    /// expect before the first one is read.
    ///
    /// `start` is called once with a number the input could actually hold,
    /// and returns the closure that reads each element. The number is the
    /// payload's count when the payload is a block that has been checked to
    /// be there, and otherwise the count clipped to the bytes left, since
    /// every element costs at least one. A container that reserves on it
    /// grows once instead of doubling its way up, and a count that lies is
    /// clipped before it can allocate on its word. Pass it through
    /// [`cautious`] all the same: the bytes left bound the *number* of
    /// elements, not what each costs in memory, and a wide element type
    /// multiplies the difference.
    ///
    /// The two-step shape is what lets one container be reserved by the
    /// first closure and filled by the second: the borrow moves from one
    /// into the other.
    ///
    /// ```
    /// use structio::beve::{Read, Reader, cautious, to_vec};
    ///
    /// let bytes = to_vec(&vec!["a".to_string(), "b".to_string()]);
    /// let mut out: Vec<String> = Vec::new();
    /// let mut r = Reader::new(&bytes);
    /// let sink = &mut out;
    /// r.read_seq_counted(|n| {
    ///     sink.reserve(cautious::<String>(n));
    ///     move |r, _| {
    ///         let mut s = String::new();
    ///         s.read(r)?;
    ///         sink.push(s);
    ///         Ok(())
    ///     }
    /// })?;
    /// assert_eq!(out, ["a", "b"]);
    /// # Ok::<(), structio::ErrorCode>(())
    /// ```
    pub fn read_seq_counted<S, F>(&mut self, start: S) -> PResult<usize>
    where
        S: FnOnce(usize) -> F,
        F: FnMut(&mut Self, usize) -> PResult<()>,
    {
        let installed = self.implied.is_some();
        let h = self.head()?;
        // The one sequence that costs no level. A complex array holds numbers
        // and nothing else, so no walk over one ever recurses, and
        // `skip_extension` charges it none either. Charging here and not there
        // would let a validator pass, at 256 containers, a document this then
        // refuses.
        //
        // `installed` keeps the test local. A synthetic element header cannot
        // equal this one, `TY_UNDEFINED` and `TY_EXTENSION` being different
        // codes, but saying so here means a reader does not have to know that
        // to see why an installed byte is never mistaken for a real extension.
        if !installed && h == header::COMPLEX {
            return self.complex_run(start);
        }
        // A generic array's header is settled before the level is charged, as
        // every other walk settles it before stepping in, so at the limit the
        // refusal is the header's everywhere.
        if header::ty(h) == header::TY_GENERIC_ARRAY {
            bare_header(h)?;
        }
        self.nested(|r| r.drive(h, start))
    }

    /// Drive the elements of a complex array, its extension header consumed.
    ///
    /// An element is a bare pair of components with no header of its own, so
    /// this installs the [synthetic one](header::complex_element) exactly as a
    /// typed array installs a real one, and for the same reason: what reads an
    /// element is the ordinary [`Read`] impl.
    fn complex_run<S, F>(&mut self, start: S) -> PResult<usize>
    where
        S: FnOnce(usize) -> F,
        F: FnMut(&mut Self, usize) -> PResult<()>,
    {
        let (class, width, pairs) = self.complex_head()?;
        // The lone form is one value, not a sequence of one.
        let n = pairs.ok_or(ErrorCode::ExpectedArray)?;
        if n > 0 {
            decodable_elements(class)?;
        }
        self.have(complex_payload(width, Some(n))?)?;
        let mut element = start(n);
        self.run(n, header::complex_element(class), &mut element)
    }

    /// The body of [`Self::read_seq`], with the header already in hand.
    fn drive<S, F>(&mut self, h: u8, start: S) -> PResult<usize>
    where
        S: FnOnce(usize) -> F,
        F: FnMut(&mut Self, usize) -> PResult<()>,
    {
        match header::ty(h) {
            header::TY_GENERIC_ARRAY => {
                let n = self.count()?;
                let mut element = start(self.honest(n));
                for i in 0..n {
                    element(self, i)?;
                }
                Ok(n)
            }
            header::TY_TYPED_ARRAY => self.typed(h, start),
            _ => Err(ErrorCode::ExpectedArray),
        }
    }

    /// Drive the elements of a typed array.
    fn typed<S, F>(&mut self, h: u8, start: S) -> PResult<usize>
    where
        S: FnOnce(usize) -> F,
        F: FnMut(&mut Self, usize) -> PResult<()>,
    {
        match self.typed_head(h)? {
            Typed::Bools(n) => {
                // Bits are read out of the payload in place; the cursor stays
                // put until the whole run is done. `typed_head` has confirmed
                // the payload is there.
                let bytes = n.div_ceil(8);
                let mut element = start(n);
                let base = self.pos;
                for i in 0..n {
                    let bit = (self.data[base + (i >> 3)] >> (i & 7)) & 1;
                    let h = if bit == 1 {
                        header::TRUE
                    } else {
                        header::FALSE
                    };
                    self.implying(h, |r| element(r, i))?;
                }
                self.pos = base + bytes;
                Ok(n)
            }
            Typed::Strings(n) => {
                let mut element = start(self.honest(n));
                self.run(n, header::STRING, &mut element)
            }
            Typed::Fixed(h, n) => {
                if n > 0 {
                    decodable_elements(h)?;
                }
                self.have(payload_len(h, n)?)?;
                let mut element = start(n);
                self.run(n, header::element_of(h), &mut element)
            }
        }
    }

    /// Drive `n` elements that each read with the implied header `elem`.
    fn run<F>(&mut self, n: usize, elem: u8, element: &mut F) -> PResult<usize>
    where
        F: FnMut(&mut Self, usize) -> PResult<()>,
    {
        for i in 0..n {
            self.implying(elem, |r| element(r, i))?;
        }
        Ok(n)
    }

    /// Consume the preamble of a value whose payload is one contiguous block
    /// of same-width elements, reporting the header its elements imply and how
    /// many of them there are.
    ///
    /// Three forms answer to that description and they differ only in their
    /// preambles: a typed numeric array, the aligned form of one, and a run of
    /// complex numbers. Deciding between them once is what keeps the two
    /// callers below from each having to know all three, and from coming to
    /// disagree about which of them is worth taking whole.
    ///
    /// `None` for anything else, for a preamble that does not parse, and for a
    /// typed array with no nesting level left to charge it: what is wrong with
    /// any of those is the ordinary path's to report, so the cursor is the
    /// caller's to put back. A complex array is not charged, being the one
    /// sequence no walk charges.
    ///
    /// `#[inline(always)]` because both callers are generic and are compiled
    /// into whichever crate reads a `Vec<f64>`, where this would otherwise be
    /// an opaque call in front of the format's hottest read. Inlined, the
    /// plain typed array is the byte test and the size decode it was before
    /// the three forms were brought together here, and only the two rarer
    /// preambles are calls.
    #[inline(always)]
    fn block_head(&mut self) -> Option<(u8, usize)> {
        if self.implied.is_some() {
            // Inside a typed array, where a nested sequence cannot occur.
            return None;
        }
        let h = *self.data.get(self.pos)?;
        self.pos += 1;
        if h == header::COMPLEX {
            // The lone form has no count and is one value rather than a run,
            // so only the run form is a block.
            let (class, _, pairs) = self.complex_head().ok()?;
            return Some((header::complex_element(class), pairs?));
        }
        if header::ty(h) != header::TY_TYPED_ARRAY {
            return None;
        }
        // Declined rather than refused, so the ordinary path, which charges
        // the same level, is what reports it.
        if !self.can_enter() {
            return None;
        }
        if header::sub(h) != header::CAT_OTHER {
            return Some((header::element_of(h), self.count().ok()?));
        }
        // Booleans and strings share the outer category and have no payload of
        // this shape. The aligned form does: it states its element type in a
        // second header and pads the payload so a reader can point straight at
        // it, which makes it the last form that should be read one element at
        // a time.
        if header::count(h) != header::OTHER_ALIGNED {
            return None;
        }
        let (inner, n) = self.aligned_head().ok()?;
        Some((header::element_of(inner), n))
    }

    /// Take a whole typed numeric array in one copy, when the stored element
    /// type is exactly `T`'s.
    ///
    /// Consumes nothing when it declines, so the caller falls through to the
    /// ordinary element-by-element path with the cursor untouched. This is the
    /// path that makes a `Vec<f64>` of a million samples a single `memcpy`.
    ///
    /// A typed array costs a nesting level here exactly as it does on the
    /// ordinary path, so one past [`MAX_DEPTH`] is declined and the ordinary
    /// path is what refuses it.
    ///
    /// A type's own bulk read is the adapted one under
    /// [`Same`](crate::Same), which forwards
    /// [`ReadAs::read_bulk`] to [`Read::read_bulk`]. Saying so here rather
    /// than writing the walk twice is what keeps the two from drifting on the
    /// one thing they both promise: that declining puts the cursor back.
    #[inline]
    pub fn try_bulk<T: Read<'de>>(&mut self, out: &mut Vec<T>) -> PResult<bool> {
        self.try_bulk_with::<crate::Same, T>(out)
    }

    /// [`Self::try_bulk`] through an adapter, over [`ReadAs::read_bulk`]
    /// rather than [`Read::read_bulk`].
    ///
    /// The reading half of [`Self::write_slice_with`]'s dispatch on
    /// [`WriteAs::ARRAY`](crate::beve::WriteAs::ARRAY), and the reason an
    /// adapted `Vec` is not stuck reading a block element by element. An
    /// adapter that leaves the hook alone declines here, which is the same
    /// answer `Vec<String>` gets from the unadapted form.
    ///
    /// The cursor is put back on the way to `false` rather than trusted to
    /// have stayed put, so an adapter that consumes and then declines is
    /// corrected instead of believed. It goes back through
    /// [`rewind`](Self::rewind), which drops any error key the declined
    /// attempt left, for the same reason: a hook that swallowed a failed read
    /// of a generated type is holding a key it did not set. As everywhere, it
    /// also leaves an element's header installed on that element and nowhere
    /// else. What is not put back is the depth, which no correct
    /// implementation moves: the walks that enter a level leave it on every
    /// exit, errors included, so even a `read_bulk` that swallowed an error
    /// returns here with it as it was.
    ///
    /// [`Self::write_slice_with`]: crate::beve::Writer::write_slice_with
    pub fn try_bulk_with<A: ReadAs<'de, T>, T>(&mut self, out: &mut Vec<T>) -> PResult<bool> {
        let start = self.pos;
        if let Some((elem, n)) = self.block_head()
            && A::read_bulk(out, n, elem, self)?
        {
            return Ok(true);
        }
        self.rewind(start);
        Ok(false)
    }

    /// Take `n` elements of payload into `out` in one copy, replacing whatever
    /// it held.
    ///
    /// The copy behind [`Self::try_bulk`], for an implementation of
    /// [`Read::read_bulk`] or [`ReadAs::read_bulk`] to call once it has
    /// decided the block is one of these. The header and the count are already
    /// consumed, so this is exactly `n * size_of::<T>()` bytes and nothing
    /// else.
    ///
    /// # Correctness
    ///
    /// The [`NumericBytes`] bound covers the layout of `T`; it says nothing
    /// about the document, and this checks nothing about it either. The caller
    /// must have established both of the things that are not properties of
    /// `T`: that the stored element type is
    /// [`T::ELEMENT`](NumericBytes::ELEMENT), and that the host is little
    /// endian.
    ///
    /// Neither is a soundness matter, which is why this is not `unsafe`: every
    /// bit pattern of a `NumericBytes` type is a value, so the worst a wrong
    /// call produces is a wrong answer. It is a silent one, though. Taking a
    /// payload of some other width leaves the cursor inside the next value,
    /// and the document reads on from there as if nothing had happened.
    #[inline]
    pub fn read_block<T: NumericBytes>(&mut self, out: &mut Vec<T>, n: usize) -> PResult<()> {
        let total = n
            .checked_mul(Block::<T>::WIDTH)
            .ok_or(ErrorCode::UnexpectedEnd)?;
        // Bounds-checked before anything is reserved, so a bogus count cannot
        // make this allocate.
        let bytes = self.take(total)?;
        out.clear();
        out.reserve(n);
        // SAFETY: `clear` then `reserve(n)` gives room for `total` bytes at a
        // pointer aligned for `T`, `bytes` is `total` bytes of a distinct
        // borrow of the input, and by the bound those bytes are `n` values of
        // `T`.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), out.as_mut_ptr().cast::<u8>(), total);
            out.set_len(n);
        }
        Ok(())
    }

    /// Borrow a whole typed numeric array out of the input, with no copy at
    /// all.
    ///
    /// `Some` is a slice pointing into the document itself. `None` consumes
    /// nothing, leaving the caller to read the value in the ordinary way,
    /// which is what [`Cow<[T]>`](std::borrow::Cow) does with it.
    ///
    /// # What a borrow needs
    ///
    /// Three things have to hold, and the document settles only the first:
    ///
    /// - The stored element type is exactly `T`'s. The width leniency every
    ///   other read has is a conversion, and a conversion is a copy: an array
    ///   of `f32` declines to be borrowed as `&[f64]`.
    /// - The payload begins on an address that is a multiple of
    ///   `align_of::<T>()`. This is what the [aligned
    ///   form](crate::beve::to_vec_aligned) is for. It pads the payload to a
    ///   multiple of the element width counted from the start of the document,
    ///   so a document that itself begins on such an address carries blocks
    ///   that do too; a plain typed array puts its payload two or three bytes
    ///   in and essentially never qualifies. What the document's own address
    ///   is depends on where it came from: a memory map is page aligned, and
    ///   the allocator behind a `Vec<u8>` gives more alignment than it
    ///   promises, but the language guarantees neither, which is why this
    ///   declines rather than fails.
    /// - The host is little endian, BEVE being a little-endian format.
    ///
    /// A borrow is also charged the nesting level the ordinary read charges a
    /// typed array, so one past [`MAX_DEPTH`] declines, and the ordinary read
    /// is what refuses it.
    ///
    /// ```
    /// use structio::beve;
    ///
    /// let doc = structio::to_beve_aligned(&vec![1.0f64, 2.0, 3.0]);
    /// let mut r = beve::Reader::new(&doc);
    /// match r.try_slice::<f64>() {
    ///     Some(block) => assert_eq!(block, [1.0, 2.0, 3.0]),
    ///     // The document did not land on an address `&[f64]` can point at.
    ///     None => assert_eq!(structio::from_beve::<Vec<f64>>(&doc).unwrap(), [1.0, 2.0, 3.0]),
    /// }
    /// ```
    pub fn try_slice<T: NumericBytes>(&mut self) -> Option<&'de [T]> {
        let start = self.pos;
        match self.borrow_block() {
            Some(block) => Some(block),
            None => {
                self.rewind(start);
                None
            }
        }
    }

    /// The body of [`Self::try_slice`], which puts the cursor back when this
    /// gives up at any of the several places it can.
    fn borrow_block<T: NumericBytes>(&mut self) -> Option<&'de [T]> {
        if cfg!(target_endian = "big") {
            return None;
        }
        let (elem, n) = self.block_head()?;
        if elem != T::ELEMENT {
            return None;
        }
        let bytes = self.take(n.checked_mul(Block::<T>::WIDTH)?).ok()?;
        let block = bytes.as_ptr().cast::<T>();
        if !block.is_aligned() {
            return None;
        }
        // SAFETY: `take` yielded `n * size_of::<T>()` initialized bytes of the
        // input, borrowed for `'de` and immutable for as long as this reader
        // exists; the pointer to them is aligned for `T` by the test above; and
        // by the `NumericBytes` bound those bytes are `n` values of `T`, the
        // element header having been confirmed to be this type's own.
        Some(unsafe { core::slice::from_raw_parts(block, n) })
    }

    // -----------------------------------------------------------------------
    // Skipping
    // -----------------------------------------------------------------------

    /// Step over the value at the cursor without interpreting it.
    ///
    /// Every BEVE value states its own extent, so this needs no lookahead and
    /// no guessing. Extensions this crate does not otherwise support are still
    /// skippable, which is what lets a document carrying a matrix in a field
    /// you do not want be read for the fields you do.
    pub fn skip_value(&mut self) -> PResult<()> {
        self.step::<false>()
    }

    /// Step over the value at the cursor, checking that every string inside it
    /// is valid UTF-8.
    ///
    /// [`Self::skip_value`] does not look at string bytes at all, a value
    /// being skipped not being one that is used. A validator is the one caller
    /// that does care.
    pub fn validate_value(&mut self) -> PResult<()> {
        self.step::<true>()
    }

    /// The walk both of the above are. `UTF8` is a constant, so the check
    /// folds away entirely on the skipping side.
    ///
    /// What counts against [`MAX_DEPTH`] is a container, not a recursion, and
    /// the rule has to be exactly the one [`Self::read_object`] and
    /// [`Self::read_seq`] apply. A typed array is what makes that distinction
    /// worth drawing: its elements are scalars, so it never recurses, but
    /// `read_seq` charges it a level all the same and so does this.
    ///
    /// Both directions of disagreement are bugs, and the second is the worse.
    /// Charging what reading does not would reject at 255 containers what
    /// reading accepts at 256. Charging less would accept, at 256, a document
    /// reading then refuses, which is a validator passing input the parser
    /// cannot take.
    fn step<const UTF8: bool>(&mut self) -> PResult<()> {
        let installed = self.implied.is_some();
        let h = self.head()?;
        // One element of a complex array, whose
        // [synthetic header](header::complex_element) the array driver
        // installed. It is the only header carrying the undefined type, and it
        // stands for two components at the width in its own fields. Read out of
        // the input the same byte is an `InvalidHeader`, which is what the
        // `installed` test preserves.
        if installed && header::ty(h) == header::TY_UNDEFINED {
            let width = header::element_width(h).ok_or(ErrorCode::InvalidHeader)?;
            return self.drop_bytes(width);
        }
        self.skip_body::<UTF8>(h)
    }

    fn skip_body<const UTF8: bool>(&mut self, h: u8) -> PResult<()> {
        match header::ty(h) {
            // Null and the booleans are the header and nothing else. Only
            // three of the four sub-codes are defined, and the byte-count
            // field must be zero, so the rest are not values to step over.
            header::TY_NULL_BOOL => match h {
                header::NULL | header::FALSE | header::TRUE => Ok(()),
                _ => Err(ErrorCode::InvalidHeader),
            },
            header::TY_NUMBER => {
                let w =
                    byte_width(header::sub(h), header::count(h)).ok_or(ErrorCode::InvalidHeader)?;
                self.drop_bytes(w)
            }
            header::TY_STRING => {
                bare_header(h)?;
                self.skip_str::<UTF8>()
            }
            header::TY_OBJECT => {
                let cat = header::sub(h);
                let width = key_width(h)?;
                let members = self.count()?;
                self.nested(|r| {
                    for _ in 0..members {
                        if cat == header::CAT_FLOAT {
                            r.skip_str::<UTF8>()?;
                        } else {
                            r.drop_bytes(width)?;
                        }
                        r.step::<UTF8>()?;
                    }
                    Ok(())
                })
            }
            header::TY_GENERIC_ARRAY => {
                bare_header(h)?;
                let n = self.count()?;
                self.nested(|r| {
                    for _ in 0..n {
                        r.step::<UTF8>()?;
                    }
                    Ok(())
                })
            }
            // Charged a level despite never recursing, because [`Self::read_seq`]
            // charges one and the two have to agree. A typed array is where the
            // deepest value in a document usually sits, so a walk that let it
            // through free would accept, one level down, exactly the documents
            // reading then refuses.
            header::TY_TYPED_ARRAY => self.nested(|r| r.skip_typed::<UTF8>(h)),
            header::TY_EXTENSION => self.skip_extension::<UTF8>(h),
            _ => Err(ErrorCode::InvalidHeader),
        }
    }

    /// Step over a `SIZE | DATA` string body, with the header already dealt
    /// with. Object keys take this path too: a key is a string without a
    /// header of its own.
    #[inline]
    fn skip_str<const UTF8: bool>(&mut self) -> PResult<()> {
        let n = self.count()?;
        let bytes = self.take(n)?;
        if UTF8 && core::str::from_utf8(bytes).is_err() {
            return Err(ErrorCode::InvalidUtf8);
        }
        Ok(())
    }

    fn skip_typed<const UTF8: bool>(&mut self, h: u8) -> PResult<()> {
        match self.typed_head(h)? {
            Typed::Bools(n) => self.drop_bytes(n.div_ceil(8)),
            Typed::Strings(n) => {
                for _ in 0..n {
                    self.skip_str::<UTF8>()?;
                }
                Ok(())
            }
            Typed::Fixed(h, n) => {
                let total = payload_len(h, n)?;
                self.drop_bytes(total)
            }
        }
    }

    /// Step over an extension value.
    ///
    /// The three that are values all state their own extent, so all of them
    /// can be stepped over, whether or not a Rust type reads them.
    fn skip_extension<const UTF8: bool>(&mut self, h: u8) -> PResult<()> {
        match header::ext_id(h) {
            // A separator between documents rather than a value, so it has no
            // place where one is expected. See `header::DELIMITER`.
            header::EXT_DELIMITER => Err(ErrorCode::InvalidHeader),
            // The deprecated type tag: an index, then the value it tagged.
            header::EXT_TYPE_TAG => {
                self.size()?;
                self.nested(|r| r.step::<UTF8>())
            }
            // A layout byte, then the extents and the data, both typed arrays.
            header::EXT_MATRIX => {
                self.drop_bytes(1)?;
                self.nested(|r| {
                    r.step::<UTF8>()?;
                    r.step::<UTF8>()
                })
            }
            // A class header, a count in the run form, and then pairs of
            // components. Charged no level: it holds numbers and nothing else,
            // so nothing here or in `read_seq` ever recurses through one.
            header::EXT_COMPLEX => {
                let (_, width, pairs) = self.complex_head()?;
                self.drop_bytes(complex_payload(width, pairs)?)
            }
            _ => Err(ErrorCode::UnsupportedFeature),
        }
    }

    // -----------------------------------------------------------------------
    // Pointers
    // -----------------------------------------------------------------------

    /// Move the cursor onto the value a [JSON Pointer] names, leaving it ready
    /// to be read.
    ///
    /// Every value in a BEVE document states its own extent, so getting to one
    /// field costs a walk over the headers in front of it rather than a parse
    /// of the values in front of it. A subtree that is not on the path is
    /// stepped over whole, and one that is a typed array is not stepped over
    /// at all: an element of it is found by multiplying. A packed-boolean array
    /// has to be present whole all the same, its padding being in its last
    /// byte and checked.
    ///
    /// Such an element carries no header of its own, so the seek installs the
    /// one the array implies, as reading the array does. The next read takes
    /// it, and a [`rewind`](Self::rewind) anywhere but onto the element takes
    /// it away, so winding back to read something else reads it as a reader
    /// that never sought would.
    ///
    /// A hand-driven reader measures depth from where it stands, so a seek
    /// leaves the reader's depth as it found it and the value it lands on is
    /// read relative to where the caller stood; [`beve::from_slice_at`] is
    /// what measures the whole document.
    ///
    /// See [`beve::from_slice_at`] for the pointer syntax and what each
    /// failure means.
    ///
    /// [JSON Pointer]: https://www.rfc-editor.org/rfc/rfc6901
    /// [`beve::from_slice_at`]: crate::beve::from_slice_at
    pub fn seek(&mut self, pointer: &str) -> PResult<()> {
        let levels = self.walk(pointer)?;
        self.depth -= levels;
        Ok(())
    }

    /// Read the value `pointer` names into `value`, giving back the levels
    /// the walk to it counted however the read exits.
    ///
    /// [`seek`](Self::seek) with the levels kept for the read, so the value is
    /// measured at the depth it sits at in the document.
    pub(crate) fn read_at<T: Read<'de>>(&mut self, pointer: &str, value: &mut T) -> PResult<()> {
        let levels = self.walk(pointer)?;
        let result = value.read(self);
        self.depth -= levels;
        result
    }

    /// Move onto the value `pointer` names, and report how many levels the
    /// containers passed through were counted. A failure counts none: the
    /// depth goes back to what it was, as a failed read's does.
    fn walk(&mut self, pointer: &str) -> PResult<u32> {
        let depth = self.depth;
        match self.descend_path(pointer) {
            Ok(()) => Ok(self.depth - depth),
            Err(code) => {
                self.depth = depth;
                Err(code)
            }
        }
    }

    /// The walk [`walk`](Self::walk) puts the depth back around.
    fn descend_path(&mut self, pointer: &str) -> PResult<()> {
        if pointer.is_empty() {
            return Ok(());
        }
        let rest = pointer.strip_prefix('/').ok_or(ErrorCode::InvalidPointer)?;
        // `"/a/"` names the empty key of `a` rather than ending in a stray
        // separator, so every `/` after the first begins a token and `split`
        // is exactly right.
        for token in rest.split('/') {
            // Checked up front, and for every token, so that a malformed
            // pointer is reported as one whatever the document happens to
            // hold at that level.
            check_escapes(token)?;
            self.descend(token)?;
        }
        Ok(())
    }

    /// Move from the container at the cursor onto the member or element
    /// `token` names, counting the container a level.
    ///
    /// The level is entered and not left: the cursor ends inside the
    /// container, and [`walk`](Self::walk) is what puts the depth back if a
    /// later step fails.
    fn descend(&mut self, token: &str) -> PResult<()> {
        let h = self.head()?;
        match header::ty(h) {
            header::TY_OBJECT => self.descend_object(h, token),
            header::TY_GENERIC_ARRAY => {
                // The header first, as `descend_object` settles its key kind
                // first. The token is decoded before the count is read, so a
                // malformed token is reported as one even where the document
                // runs out first.
                bare_header(h)?;
                let i = index(token)?;
                let n = self.count()?;
                // Before the siblings are stepped over, so that each is
                // measured from the depth it sits at.
                self.enter()?;
                if i >= n {
                    return Err(ErrorCode::NoSuchValue);
                }
                for _ in 0..i {
                    self.skip_value()?;
                }
                Ok(())
            }
            header::TY_TYPED_ARRAY => self.descend_typed(h, index(token)?),
            // No value at all, so not a scalar with no members either: the
            // document's problem, as `skip_extension` says of the same byte.
            header::TY_EXTENSION if h == header::DELIMITER => Err(ErrorCode::InvalidHeader),
            // A scalar has no members, and an extension's insides are not
            // addressable, so there is nothing here the token could name.
            header::TY_NULL_BOOL | header::TY_NUMBER | header::TY_STRING | header::TY_EXTENSION => {
                Err(ErrorCode::NoSuchValue)
            }
            // Not a type at all, which is the document's problem rather than
            // the pointer's. `skip_body` says the same of the same byte.
            _ => Err(ErrorCode::InvalidHeader),
        }
    }

    /// Find the member `token` names in the object headed by `h`.
    fn descend_object(&mut self, h: u8, token: &str) -> PResult<()> {
        let width = key_width(h)?;
        // The token is decoded once, against the key kind the object declared,
        // rather than once per member. A token that is not an integer names no
        // key of an integer-keyed object, which is a miss like any other and
        // not a malformed pointer: whether a pointer is well formed cannot
        // depend on what the document it is aimed at happens to hold.
        //
        // For the same reason the integer forms are read as `parse` takes
        // them, `+5` and `007` included, where an array index is held to the
        // RFC's canonical spelling. An array index has a spec to conform to
        // and an integer key does not, and neither spelling can mean anything
        // but the number. Nor can `-0`, which names the key `0` of an
        // unsigned-keyed object as it does of a signed one.
        let wanted = match header::sub(h) {
            header::CAT_FLOAT => Key::Str(token),
            header::CAT_SIGNED => Key::Signed(parse_int_text(token).ok_or(ErrorCode::NoSuchValue)?),
            _ => Key::Unsigned(parse_int_text(token).ok_or(ErrorCode::NoSuchValue)?),
        };

        let members = self.count()?;
        self.enter()?;
        for _ in 0..members {
            let hit = match wanted {
                Key::Str(t) => {
                    let n = self.count()?;
                    token_eq(t, self.take(n)?)
                }
                Key::Signed(v) => sign_extend(le_u128(self.take(width)?), width) == v,
                Key::Unsigned(v) => le_u128(self.take(width)?) == v,
            };
            if hit {
                return Ok(());
            }
            self.skip_value()?;
        }
        Err(ErrorCode::NoSuchValue)
    }

    /// Move onto element `i` of the typed array headed by `h`.
    ///
    /// A typed array holds a block rather than a run of values, so its
    /// elements are not walked: for a fixed width the offset is a multiply,
    /// and the header the element would have carried had it been written on
    /// its own is installed, exactly as the array driver does. Only the string
    /// form has to be walked, its elements not all being the same size.
    ///
    /// It costs a level although nothing in it recurses, because
    /// [`read_seq`](Self::read_seq) charges one and the two have to agree.
    fn descend_typed(&mut self, h: u8, i: usize) -> PResult<()> {
        self.enter()?;
        let form = self.typed_head(h)?;
        let (Typed::Bools(n) | Typed::Strings(n) | Typed::Fixed(_, n)) = form;
        if i >= n {
            return Err(ErrorCode::NoSuchValue);
        }
        match form {
            Typed::Bools(_) => {
                // `typed_head` has confirmed the whole payload is there, its
                // last byte holding the padding it checked.
                let byte = self.data[self.pos + (i >> 3)];
                // The cursor stays on the payload: a packed boolean is its
                // header and nothing else, so there is nothing after it to
                // point at.
                self.install(if (byte >> (i & 7)) & 1 == 1 {
                    header::TRUE
                } else {
                    header::FALSE
                });
                Ok(())
            }
            Typed::Strings(_) => {
                for _ in 0..i {
                    self.skip_str::<false>()?;
                }
                self.install(header::STRING);
                Ok(())
            }
            Typed::Fixed(h, _) => {
                let width =
                    byte_width(header::sub(h), header::count(h)).ok_or(ErrorCode::InvalidHeader)?;
                // A block is indexed rather than walked, which is the whole
                // reason a typed array is cheap to reach into.
                self.drop_bytes(i.checked_mul(width).ok_or(ErrorCode::UnexpectedEnd)?)?;
                self.have(width)?;
                self.install(header::element_of(h));
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pointer tokens
// ---------------------------------------------------------------------------

/// Reject a token holding a `~` that begins no escape.
///
/// RFC 6901 defines exactly two, `~0` and `~1`, and leaves any other `~` in a
/// token undefined. Treating one as a literal would let a mistyped escape
/// quietly match a key that happens to contain a tilde, so it is refused.
fn check_escapes(token: &str) -> PResult<()> {
    let t = token.as_bytes();
    let mut i = 0;
    while i < t.len() {
        if t[i] == b'~' {
            match t.get(i + 1) {
                Some(b'0' | b'1') => i += 1,
                _ => return Err(ErrorCode::InvalidPointer),
            }
        }
        i += 1;
    }
    Ok(())
}

/// Compare a pointer token against a raw key, undoing the escapes as it goes.
///
/// Decoding the token into a buffer first would mean allocating on every
/// comparison, and there are as many comparisons as the object has members.
/// Walking the two at once costs neither. Escapes are already known to be well
/// formed, [`check_escapes`] having run over this token on the way down.
fn token_eq(token: &str, key: &[u8]) -> bool {
    let t = token.as_bytes();
    let mut i = 0;
    let mut j = 0;
    while i < t.len() {
        let (want, step) = match (t[i], t.get(i + 1)) {
            (b'~', Some(b'0')) => (b'~', 2),
            (b'~', Some(b'1')) => (b'/', 2),
            (b, _) => (b, 1),
        };
        if key.get(j) != Some(&want) {
            return false;
        }
        i += step;
        j += 1;
    }
    j == key.len()
}

/// Decode an array index token.
///
/// RFC 6901 spells an index in decimal with no leading zeros. Anything else is
/// a malformed pointer rather than a miss: an array has no keys, so there is
/// nothing else such a token could have been meant to name, and reporting it
/// as absent would hide the mistake.
///
/// The exception is `-`, which the RFC defines as the position after the last
/// element. It is a well-formed token that by construction names nothing, so
/// it is absent rather than malformed, and a pointer holding one stays valid
/// against a document whose object has a key spelled `-`.
fn index(token: &str) -> PResult<usize> {
    if token == "-" {
        return Err(ErrorCode::NoSuchValue);
    }
    let t = token.as_bytes();
    let shaped =
        !t.is_empty() && t.iter().all(u8::is_ascii_digit) && !(t[0] == b'0' && t.len() > 1);
    if !shaped {
        return Err(ErrorCode::InvalidPointer);
    }
    // Spelled as an index, but too large to be one of anything in this buffer.
    token.parse().map_err(|_| ErrorCode::NoSuchValue)
}

// ---------------------------------------------------------------------------
// Width-independent number decoding
// ---------------------------------------------------------------------------

/// Bytes one key of the object headed by `h` occupies, or zero for the
/// length-prefixed string form.
///
/// Reading an object and skipping one both need this, and they have to agree:
/// if they ever disagreed about which key kinds exist or how wide one is, a
/// skipped object would leave the cursor somewhere a read object would not, and
/// the *next* member would be parsed from the wrong offset.
///
/// A header that names no key kind is an `InvalidHeader`, as it is to every
/// walk: integer keys of a width the format does not define, the fourth key
/// type, which it does not define at all, and string keys with the byte-count
/// field set. A string key carries its own length, so the specification leaves
/// that field unspecified, and it has to be zero for the reason
/// [`bare_header`] gives.
pub(crate) fn key_width(h: u8) -> PResult<usize> {
    let cat = header::sub(h);
    match cat {
        header::CAT_FLOAT if header::count(h) == 0 => Ok(0),
        header::CAT_SIGNED | header::CAT_UNSIGNED => {
            byte_width(cat, header::count(h)).ok_or(ErrorCode::InvalidHeader)
        }
        _ => Err(ErrorCode::InvalidHeader),
    }
}

/// Confirm a string or generic-array header is its type and nothing else.
///
/// Neither kind is given a `sub` or a `count`, and the specification requires
/// every bit it leaves unspecified to be zero, so a header with one set is no
/// value at all rather than another spelling of one. Accepting it would give
/// one value several encodings, which a document that is compared, hashed or
/// signed byte for byte cannot have. Every walk asks this on the header,
/// before the size, as [`key_width`] asks it of a string-keyed object.
pub(crate) fn bare_header(h: u8) -> PResult<()> {
    if header::sub(h) == 0 && header::count(h) == 0 {
        Ok(())
    } else {
        Err(ErrorCode::InvalidHeader)
    }
}

/// Bytes one element of the fixed-width typed array headed by `h` occupies.
///
/// A width the format does not define is an `InvalidHeader`, as it is for a
/// lone number of that width.
pub(crate) fn fixed_width(h: u8) -> PResult<usize> {
    byte_width(header::sub(h), header::count(h)).ok_or(ErrorCode::InvalidHeader)
}

/// Confirm a block's elements are of a type something can decode, before its
/// payload is looked for.
///
/// `h` is a typed array's header or a complex value's class header, either of
/// which states the element type once for the whole block. A 128-bit float is
/// [`UnsupportedFeature`](ErrorCode::UnsupportedFeature) to the first element
/// read, so a driver that confirmed the payload first would call a truncated
/// block `UnexpectedEnd` where `Value` and the transcode, which ask
/// [`header::decodable_width`] of the header, call it `UnsupportedFeature`.
/// Asked only where there is an element to refuse: an empty block reads.
fn decodable_elements(h: u8) -> PResult<()> {
    header::decodable_width(header::sub(h), header::count(h)).map(drop)
}

/// Refuse the last byte of an `n`-element packed-boolean array if it sets a
/// bit past the last element.
///
/// The specification requires those high bits to be zero. Read as anything
/// else, each one set would give the same array another encoding, which a
/// document compared, hashed or signed byte for byte cannot have. A count
/// that fills its last byte has no padding to check.
#[inline]
pub(crate) fn bool_padding(n: usize, last: u8) -> PResult<()> {
    let used = n & 7;
    if used != 0 && last >> used != 0 {
        return Err(ErrorCode::InvalidPadding);
    }
    Ok(())
}

/// Refuse an aligned block's `PADDING_LENGTH` if it is not below the
/// element's alignment, which for every numeric type is its width.
///
/// The specification bounds it to `0..alignment` and leaves the padding's
/// contents unspecified, so those are not looked at. Nor is the length
/// required to be the one an encoder would choose: that depends on the
/// block's offset from the start of the whole message, which a reader handed
/// a slice of it, a streamed value, or a pointer's target cannot know. What
/// is refused is a length no placement could call for.
#[inline]
pub(crate) fn aligned_padding(pad: u8, width: usize) -> PResult<()> {
    if usize::from(pad) >= width {
        return Err(ErrorCode::InvalidPadding);
    }
    Ok(())
}

/// Bytes a fixed-width payload of `n` elements described by `h` occupies.
pub(crate) fn payload_len(h: u8, n: usize) -> PResult<usize> {
    n.checked_mul(fixed_width(h)?)
        .ok_or(ErrorCode::UnexpectedEnd)
}

/// Bytes of payload behind a complex value's preamble, as
/// [`Reader::complex_head`] reported it.
///
/// One pair for the lone form, `n` for the run form, and two components in
/// either. `2 * width` cannot overflow: no class header describes a component
/// wider than sixteen bytes.
pub(crate) fn complex_payload(width: usize, pairs: Option<usize>) -> PResult<usize> {
    pairs
        .unwrap_or(1)
        .checked_mul(2 * width)
        .ok_or(ErrorCode::UnexpectedEnd)
}

/// Confirm a typed array holds one-byte integers.
///
/// An element type the format does not define is no array of anything, and is
/// refused as every other walk refuses it rather than as the wrong kind.
fn byte_elements(h: u8) -> PResult<()> {
    fixed_width(h)?;
    match header::sub(h) {
        header::CAT_SIGNED | header::CAT_UNSIGNED if header::count(h) == 0 => Ok(()),
        _ => Err(ErrorCode::ExpectedBytes),
    }
}

/// Little-endian load of up to sixteen bytes.
#[inline]
/// Turn a number's payload into an `f64`.
///
/// Integers convert, which is what makes an `f64` field able to read a document
/// whose producer happened to have an integral value in it, the same way `1`
/// parses into an `f64` from JSON.
fn widen(cat: u8, code: u8, bytes: &[u8]) -> PResult<f64> {
    match cat {
        header::CAT_FLOAT => match code {
            // No 8-bit float exists, so the two narrowest codes are the two
            // 16-bit ones. See `header::byte_width`.
            0 => Ok(bf16_to_f32(u16::from_le_bytes([bytes[0], bytes[1]])) as f64),
            1 => Ok(f16_to_f32(u16::from_le_bytes([bytes[0], bytes[1]])) as f64),
            2 => Ok(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64),
            3 => Ok(f64::from_le_bytes(bytes.try_into().expect("8 bytes"))),
            // f128 has no Rust counterpart to land in.
            _ => Err(ErrorCode::UnsupportedFeature),
        },
        header::CAT_UNSIGNED => Ok(le_u128(bytes) as f64),
        header::CAT_SIGNED => Ok(sign_extend(le_u128(bytes), bytes.len()) as f64),
        _ => Err(ErrorCode::ExpectedNumber),
    }
}

pub(crate) fn le_u128(bytes: &[u8]) -> u128 {
    let mut v: u128 = 0;
    for (i, &b) in bytes.iter().enumerate() {
        v |= (b as u128) << (8 * i);
    }
    v
}

/// Reinterpret the low `width` bytes of `v` as a two's-complement signed value.
#[inline]
pub(crate) fn sign_extend(v: u128, width: usize) -> i128 {
    let bits = 8 * width;
    if bits >= 128 {
        return v as i128;
    }
    let shift = 128 - bits;
    ((v << shift) as i128) >> shift
}

/// A brain float is the top half of an `f32`, so widening is a shift.
#[inline]
pub(crate) fn bf16_to_f32(bits: u16) -> f32 {
    f32::from_bits((bits as u32) << 16)
}

/// IEEE binary16 to `f32`.
///
/// Every binary16 is exactly representable as an `f32`, including the
/// subnormals, which is why the subnormal branch renormalizes rather than
/// rounding: the exponent range of `f32` is wide enough to hold them all as
/// ordinary numbers.
pub(crate) fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) as u32) << 31;
    let exp = ((bits >> 10) & 0x1f) as u32;
    let man = (bits & 0x3ff) as u32;
    let rest = match exp {
        0 if man == 0 => 0,
        0 => {
            // Subnormal: the value is `man * 2^-24`. Its leading one becomes
            // the implicit bit, and the exponent falls out of where that one
            // sits. `man` is non-zero here, so `k` is well defined.
            let k = 31 - man.leading_zeros();
            let e = k + 127 - 24;
            (e << 23) | ((man << (23 - k)) & 0x007f_ffff)
        }
        // All ones: infinity or NaN, with the payload widened in place.
        0x1f => 0x7f80_0000 | (man << 13),
        _ => ((exp + 127 - 15) << 23) | (man << 13),
    };
    f32::from_bits(sign | rest)
}
