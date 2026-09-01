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
use crate::beve::traits::{Read, ReadArray, ReadAs, ReadEnum, ReadObject};
use crate::error::{ErrorCode, PResult};
use crate::options::{Options, Standard};
use crate::traits::Fields;

/// Deepest nesting accepted, so a hostile document cannot exhaust the stack.
pub const MAX_DEPTH: u32 = 256;

/// A BEVE object key, in whichever of the three forms the object declared.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key<'de> {
    Str(&'de str),
    Signed(i128),
    Unsigned(u128),
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
    /// The header the next value must be read with, when it carries none of
    /// its own. See the module docs.
    implied: Option<u8>,
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
            implied: None,
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
            implied: Some(implied),
            options: PhantomData,
        }
    }

    /// Byte offset of the cursor, which is where an error is reported from.
    #[inline]
    pub fn position(&self) -> usize {
        self.pos
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
    #[inline]
    pub fn rewind(&mut self, to: usize) {
        // Clamping is also what keeps `pos <= data.len()`, which every bounds
        // test in here is written against.
        self.pos = to.min(self.pos);
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

    #[inline(always)]
    pub(crate) fn enter(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            Err(ErrorCode::ExceededMaxDepth)
        } else {
            Ok(())
        }
    }

    #[inline(always)]
    pub(crate) fn leave(&mut self) {
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
    fn aligned_head(&mut self) -> PResult<(u8, usize)> {
        let inner = self.head()?;
        if header::ty(inner) != header::TY_TYPED_ARRAY || header::sub(inner) == header::CAT_OTHER {
            return Err(ErrorCode::InvalidHeader);
        }
        let n = self.count()?;
        let pad = self.take(1)?[0] as usize;
        self.drop_bytes(pad)?;
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
    pub(crate) fn typed_head(&mut self, h: u8) -> PResult<Typed> {
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
            _ => Ok(Typed::Fixed(h, self.count()?)),
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
            Some(_) => Err(ErrorCode::ExpectedComplex),
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
        self.implied = Some(elem);
        re.read(self)?;
        self.implied = Some(elem);
        im.read(self)?;
        // As `run` does: an installed header that nothing consumed must not
        // outlive the value it was installed for.
        self.implied = None;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Scalars
    // -----------------------------------------------------------------------

    #[inline]
    pub fn read_bool(&mut self) -> PResult<bool> {
        match self.head()? {
            header::TRUE => Ok(true),
            header::FALSE => Ok(false),
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
        let width = byte_width(cat, header::count(h)).ok_or(ErrorCode::InvalidHeader)?;
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
        let width = byte_width(cat, code).ok_or(ErrorCode::InvalidHeader)?;
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
        let bytes = self.take(n)?;
        core::str::from_utf8(bytes).map_err(|_| ErrorCode::InvalidUtf8)
    }

    /// Borrow a byte array straight out of the input.
    ///
    /// Accepts a typed array of one-byte elements, of either signedness, and
    /// the aligned form of the same. A wider element type is not a run of
    /// bytes and is reported rather than reinterpreted.
    pub fn read_bytes(&mut self) -> PResult<&'de [u8]> {
        let h = self.head()?;
        if header::ty(h) != header::TY_TYPED_ARRAY {
            return Err(ErrorCode::ExpectedArray);
        }
        if header::sub(h) != header::CAT_OTHER {
            byte_elements(h)?;
            let n = self.count()?;
            return self.take(n);
        }
        // The aligned form states its element type in a second header, and
        // pads the payload so a reader can point at it directly.
        if header::count(h) != header::OTHER_ALIGNED {
            return Err(ErrorCode::ExpectedBytes);
        }
        let (inner, n) = self.aligned_head()?;
        byte_elements(inner)?;
        self.take(n)
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
        if header::sub(h) != header::CAT_FLOAT {
            // Categories 1 and 2 are integer keys, which no `object!` struct
            // has: its keys are names.
            return Err(ErrorCode::UnsupportedKeyType);
        }
        let members = self.count()?;
        self.enter()?;

        let map = T::MAP;
        let fields = map.n as usize;
        // One bit per field filled, compared once the object ends against the
        // fields that had to be there. Never written, and so never read, unless
        // the policy or the type asks for one.
        let mut seen = 0u64;
        for _ in 0..members {
            let n = self.count()?;
            // Where the key's bytes begin, so a refusal can point at them
            // rather than at the value they introduced. Dead, and gone, under
            // a policy that cannot refuse.
            let at = self.pos;
            let key = self.take(n)?;
            let index = map.lookup_sized(T::KEYS, key);
            let matched = index < fields && T::read_field(value, index, key, self)?;
            if Fields::<O, T>::TRACK && matched {
                seen |= Fields::<O, T>::seen(index);
            }
            if !matched {
                if O::ERROR_ON_UNKNOWN_KEYS {
                    self.pos = at;
                    return Err(ErrorCode::UnknownKey);
                }
                self.skip_value()?;
            }
        }

        self.leave();
        let mask = Fields::<O, T>::MASK;
        if seen & mask != mask {
            // Back to the object's header: the cursor is past the object by
            // now, and what is incomplete is the object, not what follows it.
            self.pos = open;
            return Err(ErrorCode::MissingKey);
        }
        Ok(())
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
                let name = self.str_body()?.as_bytes();
                // The hash only proposes a variant; `read_name` confirms the
                // name itself and may still decline.
                let index = T::MAP.lookup_sized(T::VARIANTS, name);
                if index >= T::MAP.n as usize || !T::read_name(value, index, name)? {
                    self.pos = open;
                    return Err(ErrorCode::UnknownVariant);
                }
                Ok(())
            }
            header::TY_OBJECT => {
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
                self.enter()?;
                let n = self.count()?;
                // Where the name's bytes begin, so a refusal points at them
                // rather than at the value they introduced.
                let at = self.pos;
                let name = self.take(n)?;
                let index = T::MAP.lookup_sized(T::VARIANTS, name);
                if index >= T::MAP.n as usize || !T::read_payload(value, index, name, self)? {
                    self.pos = at;
                    return Err(ErrorCode::UnknownVariant);
                }
                self.leave();
                Ok(())
            }
            _ => {
                self.pos = open;
                Err(ErrorCode::ExpectedVariant)
            }
        }
    }

    /// Drive a BEVE object as a map, calling `entry` with each key.
    ///
    /// The key arrives already typed: BEVE stores integer keys as integers, so
    /// a `HashMap<u32, _>` round-trips without the stringification JSON forces.
    pub fn read_map<F>(&mut self, mut entry: F) -> PResult<()>
    where
        F: FnMut(&mut Self, Key<'de>) -> PResult<()>,
    {
        let h = self.head()?;
        if header::ty(h) != header::TY_OBJECT {
            return Err(ErrorCode::ExpectedObject);
        }
        let cat = header::sub(h);
        let width = key_width(h)?;
        let members = self.count()?;
        self.enter()?;

        for _ in 0..members {
            let key = match cat {
                header::CAT_FLOAT => Key::Str(self.str_body()?),
                header::CAT_SIGNED => Key::Signed(sign_extend(le_u128(self.take(width)?), width)),
                _ => Key::Unsigned(le_u128(self.take(width)?)),
            };
            entry(self, key)?;
        }

        self.leave();
        Ok(())
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
    pub fn read_seq<F>(&mut self, mut element: F) -> PResult<usize>
    where
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
            return self.complex_run(&mut element);
        }
        self.enter()?;
        let n = self.drive(h, &mut element)?;
        self.leave();
        Ok(n)
    }

    /// Drive the elements of a complex array, its extension header consumed.
    ///
    /// An element is a bare pair of components with no header of its own, so
    /// this installs the [synthetic one](header::complex_element) exactly as a
    /// typed array installs a real one, and for the same reason: what reads an
    /// element is the ordinary [`Read`] impl.
    fn complex_run<F>(&mut self, element: &mut F) -> PResult<usize>
    where
        F: FnMut(&mut Self, usize) -> PResult<()>,
    {
        let (class, width, pairs) = self.complex_head()?;
        // The lone form is one value, not a sequence of one.
        let n = pairs.ok_or(ErrorCode::ExpectedArray)?;
        self.have(complex_payload(width, Some(n))?)?;
        self.run(n, header::complex_element(class), element)
    }

    /// The body of [`Self::read_seq`], with the header already in hand.
    fn drive<F>(&mut self, h: u8, element: &mut F) -> PResult<usize>
    where
        F: FnMut(&mut Self, usize) -> PResult<()>,
    {
        match header::ty(h) {
            header::TY_GENERIC_ARRAY => {
                let n = self.count()?;
                for i in 0..n {
                    element(self, i)?;
                }
                Ok(n)
            }
            header::TY_TYPED_ARRAY => self.typed(h, element),
            _ => Err(ErrorCode::ExpectedArray),
        }
    }

    /// Drive the elements of a typed array.
    fn typed<F>(&mut self, h: u8, element: &mut F) -> PResult<usize>
    where
        F: FnMut(&mut Self, usize) -> PResult<()>,
    {
        match self.typed_head(h)? {
            Typed::Bools(n) => {
                // Bits are read out of the payload in place; the cursor stays
                // put until the whole run is done.
                let bytes = n.div_ceil(8);
                self.have(bytes)?;
                let base = self.pos;
                for i in 0..n {
                    let bit = (self.data[base + (i >> 3)] >> (i & 7)) & 1;
                    self.implied = Some(if bit == 1 {
                        header::TRUE
                    } else {
                        header::FALSE
                    });
                    element(self, i)?;
                }
                self.implied = None;
                self.pos = base + bytes;
                Ok(n)
            }
            Typed::Strings(n) => self.run(n, header::STRING, element),
            Typed::Fixed(h, n) => {
                self.have(payload_len(h, n)?)?;
                self.run(n, header::element_of(h), element)
            }
        }
    }

    /// Drive `n` elements that each read with the implied header `elem`.
    fn run<F>(&mut self, n: usize, elem: u8, element: &mut F) -> PResult<usize>
    where
        F: FnMut(&mut Self, usize) -> PResult<()>,
    {
        for i in 0..n {
            self.implied = Some(elem);
            element(self, i)?;
        }
        self.implied = None;
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
    /// `None` for anything else, and for a preamble that does not parse: what
    /// is wrong with it is the ordinary path's to report, so the cursor is the
    /// caller's to put back.
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
    /// corrected instead of believed. What is not put back is the implied
    /// element header and the depth, which no correct implementation moves:
    /// both are restored by the walks that set them, and only a `read_bulk`
    /// that swallowed an error could return here with either disturbed.
    ///
    /// [`Self::write_slice_with`]: crate::beve::Writer::write_slice_with
    pub fn try_bulk_with<A: ReadAs<'de, T>, T>(&mut self, out: &mut Vec<T>) -> PResult<bool> {
        let start = self.pos;
        if let Some((elem, n)) = self.block_head()
            && A::read_bulk(out, n, elem, self)?
        {
            return Ok(true);
        }
        self.pos = start;
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
                self.pos = start;
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
            header::TY_STRING => self.skip_str::<UTF8>(),
            header::TY_OBJECT => {
                let cat = header::sub(h);
                let width = key_width(h)?;
                let members = self.count()?;
                self.enter()?;
                for _ in 0..members {
                    if cat == header::CAT_FLOAT {
                        self.skip_str::<UTF8>()?;
                    } else {
                        self.drop_bytes(width)?;
                    }
                    self.step::<UTF8>()?;
                }
                self.leave();
                Ok(())
            }
            header::TY_GENERIC_ARRAY => {
                let n = self.count()?;
                self.enter()?;
                for _ in 0..n {
                    self.step::<UTF8>()?;
                }
                self.leave();
                Ok(())
            }
            // Charged a level despite never recursing, because [`Self::read_seq`]
            // charges one and the two have to agree. A typed array is where the
            // deepest value in a document usually sits, so a walk that let it
            // through free would accept, one level down, exactly the documents
            // reading then refuses.
            header::TY_TYPED_ARRAY => {
                self.enter()?;
                self.skip_typed::<UTF8>(h)?;
                self.leave();
                Ok(())
            }
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
    /// None of these are read into Rust types, but all of them state their own
    /// extent, so all of them can be stepped over.
    fn skip_extension<const UTF8: bool>(&mut self, h: u8) -> PResult<()> {
        match header::ext_id(h) {
            // A delimiter is a marker with no body.
            header::EXT_DELIMITER => Ok(()),
            // The deprecated type tag: an index, then the value it tagged.
            header::EXT_TYPE_TAG => {
                self.size()?;
                self.enter()?;
                self.step::<UTF8>()?;
                self.leave();
                Ok(())
            }
            // A layout byte, then the extents and the data, both typed arrays.
            header::EXT_MATRIX => {
                self.drop_bytes(1)?;
                self.enter()?;
                self.step::<UTF8>()?;
                self.step::<UTF8>()?;
                self.leave();
                Ok(())
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
    /// at all: an element of it is found by multiplying.
    ///
    /// See [`beve::from_slice_at`] for the pointer syntax and what each
    /// failure means.
    ///
    /// [JSON Pointer]: https://www.rfc-editor.org/rfc/rfc6901
    /// [`beve::from_slice_at`]: crate::beve::from_slice_at
    pub fn seek(&mut self, pointer: &str) -> PResult<()> {
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
    /// `token` names.
    fn descend(&mut self, token: &str) -> PResult<()> {
        let h = self.head()?;
        match header::ty(h) {
            header::TY_OBJECT => self.descend_object(h, token),
            header::TY_GENERIC_ARRAY => {
                // Decoded before the count is read, so a malformed token is
                // reported as one even where the document runs out first.
                let i = index(token)?;
                let n = self.count()?;
                if i >= n {
                    return Err(ErrorCode::NoSuchValue);
                }
                for _ in 0..i {
                    self.skip_value()?;
                }
                Ok(())
            }
            header::TY_TYPED_ARRAY => self.descend_typed(h, index(token)?),
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
        // but the number.
        let wanted = match header::sub(h) {
            header::CAT_FLOAT => Key::Str(token),
            header::CAT_SIGNED => Key::Signed(token.parse().map_err(|_| ErrorCode::NoSuchValue)?),
            _ => Key::Unsigned(token.parse().map_err(|_| ErrorCode::NoSuchValue)?),
        };

        let members = self.count()?;
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
    fn descend_typed(&mut self, h: u8, i: usize) -> PResult<()> {
        let form = self.typed_head(h)?;
        let (Typed::Bools(n) | Typed::Strings(n) | Typed::Fixed(_, n)) = form;
        if i >= n {
            return Err(ErrorCode::NoSuchValue);
        }
        match form {
            Typed::Bools(_) => {
                self.have((i >> 3) + 1)?;
                let byte = self.data[self.pos + (i >> 3)];
                // The cursor stays on the payload: a packed boolean is its
                // header and nothing else, so there is nothing after it to
                // point at.
                self.implied = Some(if (byte >> (i & 7)) & 1 == 1 {
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
                self.implied = Some(header::STRING);
                Ok(())
            }
            Typed::Fixed(h, _) => {
                let width =
                    byte_width(header::sub(h), header::count(h)).ok_or(ErrorCode::InvalidHeader)?;
                // A block is indexed rather than walked, which is the whole
                // reason a typed array is cheap to reach into.
                self.drop_bytes(i.checked_mul(width).ok_or(ErrorCode::UnexpectedEnd)?)?;
                self.have(width)?;
                self.implied = Some(header::element_of(h));
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
/// formed, [`check_escapes`] having run over this token in [`Reader::seek`].
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
pub(crate) fn key_width(h: u8) -> PResult<usize> {
    let cat = header::sub(h);
    match cat {
        header::CAT_FLOAT => Ok(0),
        header::CAT_SIGNED | header::CAT_UNSIGNED => {
            byte_width(cat, header::count(h)).ok_or(ErrorCode::InvalidHeader)
        }
        _ => Err(ErrorCode::UnsupportedKeyType),
    }
}

/// Bytes a fixed-width payload of `n` elements described by `h` occupies.
pub(crate) fn payload_len(h: u8, n: usize) -> PResult<usize> {
    let width = byte_width(header::sub(h), header::count(h)).ok_or(ErrorCode::InvalidHeader)?;
    n.checked_mul(width).ok_or(ErrorCode::UnexpectedEnd)
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
fn byte_elements(h: u8) -> PResult<()> {
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
