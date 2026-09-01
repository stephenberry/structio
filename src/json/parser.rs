//! The parse cursor.
//!
//! [`Parser`] walks the input directly into the destination. There is no
//! intermediate document, no token stream, and no per-value allocation: a
//! field's bytes are converted once, straight into the struct member that will
//! hold them.
//!
//! Input is always a `&str`, so the whole document is known to be valid UTF-8
//! before parsing starts. String values can therefore be sliced out and handed
//! back without revalidation, and unescaping only has to produce valid UTF-8
//! for the escapes it expands.

use core::marker::PhantomData;

use crate::error::{ErrorCode, PResult};
use crate::json::traits::{Read, ReadArray, ReadEnum, ReadObject};
use crate::num::atof::{parse_float, scan_number};
use crate::num::atoi::{parse_i64, parse_u64, reject_float_tail};
use crate::options::{Options, Standard};
use crate::swar::{escape_mask, find_byte, first_match, load_u64, needs_escape};
use crate::traits::{Fields, Keys};

/// Nesting limit, so a hostile document cannot exhaust the stack.
pub const MAX_DEPTH: u32 = 256;

/// A cursor over a JSON document.
///
/// `O` is the [read policy](crate::Options). It decides nothing about the
/// cursor's state and is never constructed; it is read through `O::CONSTANT`
/// at the points that consult a setting, so an unselected behaviour costs no
/// code. It defaults to [`Standard`] where the type is written out.
pub struct Parser<'de, O: Options = Standard> {
    data: &'de [u8],
    idx: usize,
    depth: u32,
    /// The key to attach to the failure this parse is about to return. See
    /// [`set_error_key`](Parser::set_error_key).
    error_key: Option<&'static str>,
    /// `fn() -> O` rather than `O`, so the parser's auto traits follow what it
    /// actually holds rather than a policy type it never contains.
    options: PhantomData<fn() -> O>,
}

impl<'de> Parser<'de> {
    /// Wrap an input document, read under [`Standard`].
    ///
    /// This is the constructor to reach for. Hand-driving a parser is usually
    /// for reaching a document's bytes directly, where no setting applies;
    /// [`read_object`](Self::read_object) is the exception, and reads under
    /// [`Standard`] here like everything else.
    /// [`with_options`](Self::with_options) names a different policy, and is
    /// what the `_with` entry points use.
    ///
    /// ```
    /// use structio::json::Parser;
    ///
    /// let p = Parser::new("{}");
    /// assert_eq!(p.position(), 0);
    /// ```
    #[inline]
    pub fn new(input: &'de str) -> Self {
        Self::with_options(input)
    }
}

impl<'de, O: Options> Parser<'de, O> {
    /// Wrap an input document, read under the policy `O`.
    ///
    /// The policy is named once here and inferred everywhere after. A
    /// defaulted type parameter fills in a *type*; it does not tell inference
    /// what an associated function's `Self` is, which is why the default is
    /// reached through [`new`](Self::new) rather than by leaving `O` off.
    ///
    /// ```
    /// use structio::{SkipUnknown, json::Parser};
    ///
    /// let p = Parser::<SkipUnknown>::with_options("{}");
    /// assert_eq!(p.position(), 0);
    /// ```
    #[inline]
    pub fn with_options(input: &'de str) -> Self {
        Parser {
            data: input.as_bytes(),
            idx: 0,
            depth: 0,
            error_key: None,
            options: PhantomData,
        }
    }

    /// Byte offset of the cursor, used to locate errors.
    #[inline(always)]
    pub fn position(&self) -> usize {
        self.idx
    }

    /// The key set for the failure being returned, if there is one.
    ///
    /// The companion of [`position`](Self::position): where that says which
    /// byte, this says which key. An entry point reads both when a read comes
    /// back `Err`, and they become [`Error::index`] and [`Error::key`].
    ///
    /// [`Error::index`]: crate::Error::index
    /// [`Error::key`]: crate::Error::key
    #[inline(always)]
    pub fn error_key(&self) -> Option<&'static str> {
        self.error_key
    }

    /// Name the key the failure about to be returned is about.
    ///
    /// The counterpart of [`rewind`](Self::rewind) for a hand-written [`Read`]
    /// impl, and for the same case: a reader that discovers at the end of an
    /// object that a member never arrived wants to name the object *and* to
    /// name the member, the offset alone being able to do only the first. This
    /// is what [`read_object`](Self::read_object) does for
    /// [`ErrorCode::MissingKey`], and what [`Matrix`](crate::Matrix) does by
    /// hand.
    ///
    /// **Set it after any [`rewind`](Self::rewind), on the branch that is
    /// returning `Err`.** A successful read does not clear the key, because
    /// clearing would mean a store on every object read; what clears it is
    /// [`rewind`](Self::rewind), which is how a reader abandons a read it is
    /// discarding. Setting a key and then winding back loses it, and that is
    /// the right way round.
    ///
    /// Only [`&'static str`](str) goes here, so an
    /// [`Error`](crate::Error) still outlives the document. A name read out of
    /// the input does not qualify, and does not need to: the cursor can be
    /// wound back to it instead, which is what the unknown-key and
    /// unknown-variant paths do.
    ///
    /// [`ErrorCode::MissingKey`]: crate::ErrorCode::MissingKey
    #[inline(always)]
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
    /// object that a member never arrived wants to name the object, not the
    /// byte that closed it. That is exactly what [`read_object`](Self::read_object)
    /// does for [`Options::ERROR_ON_MISSING_KEYS`],
    /// and what [`Matrix`](crate::Matrix) does by hand.
    ///
    /// The cursor never moves forward: a position ahead of it leaves it where
    /// it is. Winding forward would step over input without reading it, which
    /// is not something a caller could mean by "rewind".
    ///
    /// ```
    /// use structio::json::Parser;
    ///
    /// let mut p = Parser::new(r#"{"a":1}"#);
    /// let open = p.position();
    /// p.skip_value().unwrap();
    /// assert_eq!(p.position(), 7);
    ///
    /// p.rewind(open);
    /// assert_eq!(p.position(), open);
    ///
    /// // Forward is not a rewind, so nothing happens.
    /// p.rewind(4);
    /// assert_eq!(p.position(), open);
    /// ```
    /// Any key [`set_error_key`](Self::set_error_key) left is dropped,
    /// whoever set it. Winding back is how a reader abandons what it just
    /// read, and the key is part of what it read: a speculating reader that
    /// discards a failed read of a *generated* type would otherwise carry off
    /// a key the generated reader set behind its back, and have no way to know
    /// it was there. So the abandoning is what clears it, rather than a rule
    /// the abandoning reader has to remember.
    #[inline]
    pub fn rewind(&mut self, to: usize) {
        // Clamping is also what keeps `idx <= data.len()`, which every bounds
        // test in here is written against.
        self.idx = to.min(self.idx);
        self.error_key = None;
    }

    /// Remaining input, starting at the cursor.
    #[inline(always)]
    pub fn rest(&self) -> &'de [u8] {
        // SAFETY-free: `idx` never advances past `data.len()`.
        &self.data[self.idx..]
    }

    #[inline(always)]
    fn remaining(&self) -> usize {
        self.data.len() - self.idx
    }

    #[inline(always)]
    pub(crate) fn peek(&self) -> Option<u8> {
        self.data.get(self.idx).copied()
    }

    /// Skip JSON whitespace: space, tab, newline, carriage return.
    ///
    /// Under [`Options::ALLOW_COMMENTS`] a complete `//` or `/* */` comment is
    /// whitespace too, and runs of the two interleave freely. An incomplete
    /// one is not consumed, so the cursor stops on the `/` and whatever the
    /// caller expected there is reported against it.
    #[inline(always)]
    pub fn skip_ws(&mut self) {
        self.idx = skip_ws_at::<O>(self.data, self.idx);
    }

    /// Consume `b` if it is next.
    #[inline(always)]
    pub fn try_byte(&mut self, b: u8) -> bool {
        if self.idx < self.data.len() && self.data[self.idx] == b {
            self.idx += 1;
            true
        } else {
            false
        }
    }

    /// Consume `b`, or fail with `code`.
    #[inline(always)]
    pub fn expect(&mut self, b: u8, code: ErrorCode) -> PResult<()> {
        if self.idx < self.data.len() && self.data[self.idx] == b {
            self.idx += 1;
            Ok(())
        } else if self.idx >= self.data.len() {
            Err(ErrorCode::UnexpectedEnd)
        } else {
            Err(code)
        }
    }

    /// Whitespace, `:`, whitespace. Called once per object member.
    #[inline(always)]
    pub fn colon(&mut self) -> PResult<()> {
        self.skip_ws();
        self.expect(b':', ErrorCode::ExpectedColon)?;
        self.skip_ws();
        Ok(())
    }

    /// Consume the separator between container members: either a comma, or the
    /// closing byte that ends the container.
    ///
    /// Returns `true` to keep looping and `false` once the container is closed,
    /// leaving the cursor past whichever byte it consumed. Every object, array,
    /// map, and skip loop ends the same way, and so does each container the
    /// [prettifier](crate::prettify) lays out, so they all end here, and all
    /// report the same error when the document holds neither byte.
    #[inline(always)]
    pub(crate) fn comma_or_close(&mut self, close: u8) -> PResult<bool> {
        self.skip_ws();
        if self.try_byte(b',') {
            self.skip_ws();
            return Ok(true);
        }
        if self.try_byte(close) {
            return Ok(false);
        }
        Err(if self.idx >= self.data.len() {
            ErrorCode::UnexpectedEnd
        } else {
            ErrorCode::ExpectedComma
        })
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

    /// Confirm that the key at the cursor is exactly `key`, and step past it
    /// and its closing quote.
    ///
    /// Called from macro-generated code with a literal, so `key.len()` is a
    /// constant and the comparison inlines to a fixed-size compare rather than
    /// a `memcmp` call. This is the check that makes the perfect hash safe: the
    /// hash only proposes a candidate, and an unknown key that collides with an
    /// occupied bucket is rejected here.
    #[inline(always)]
    pub fn match_key(&mut self, key: &'static str) -> bool {
        let k = key.as_bytes();
        let n = k.len();
        let i = self.idx;
        // `i + n < len` covers both the key bytes and the closing quote.
        if i + n < self.data.len() && self.data[i + n] == b'"' && &self.data[i..i + n] == k {
            self.idx = i + n + 1;
            true
        } else {
            false
        }
    }

    // -----------------------------------------------------------------------
    // Structural
    // -----------------------------------------------------------------------

    /// Read a JSON object into a type declared with `object!`.
    ///
    /// One iteration per member: hash the key to a candidate index, let the
    /// generated dispatch confirm it and parse the value, then take the comma
    /// or the closing brace.
    pub fn read_object<T: ReadObject<'de>>(&mut self, value: &mut T) -> PResult<()> {
        self.skip_ws();
        // Where the object begins, so a member it never got to can be reported
        // against the object rather than against the byte that closed it. Dead,
        // and gone, under a policy that requires nothing.
        let open = self.idx;
        self.expect(b'{', ErrorCode::ExpectedBrace)?;
        self.enter()?;
        self.skip_ws();

        // One bit per field filled, compared once the object ends against the
        // fields that had to be there. Never written, and so never read, unless
        // the policy or the type asks for one.
        let mut seen = 0u64;

        if self.try_byte(b'}') {
            self.leave();
            return self.require_fields::<T>(seen, open);
        }

        let map = T::MAP;
        let n = map.n as usize;

        loop {
            self.expect(b'"', ErrorCode::ExpectedQuote)?;

            let index = map.lookup(T::KEYS, self.rest());
            let matched = if index < n {
                T::read_field(value, index, self)?
            } else {
                false
            };
            if Fields::<O, T>::TRACK && matched {
                seen |= Fields::<O, T>::seen(index);
            }
            if !matched {
                if O::ERROR_ON_UNKNOWN_KEYS {
                    // `match_key` fails identically for a key that differs
                    // and for one the input ended in the middle of, so
                    // reaching here is not yet evidence of a schema mismatch.
                    // Walking the key to its closing quote is what tells the
                    // two apart, and it reports the truncation itself.
                    let key = self.idx;
                    self.skip_string_body()?;
                    // Back to the first byte of the key, so the position this
                    // error carries names what was not recognized.
                    self.idx = key;
                    return Err(ErrorCode::UnknownKey);
                }
                self.skip_unknown_member()?;
            }

            if !self.comma_or_close(b'}')? {
                self.leave();
                return self.require_fields::<T>(seen, open);
            }
        }
    }

    /// Refuse an object that ended with a required field never filled.
    ///
    /// Compiles to nothing where nothing is required, the mask then being a
    /// constant zero and `seen` a constant zero with it.
    #[inline]
    fn require_fields<T: Keys>(&mut self, seen: u64, open: usize) -> PResult<()> {
        let mask = Fields::<O, T>::MASK;
        if seen & mask != mask {
            // Back to the opening brace: the cursor is past the object by now,
            // and what is incomplete is the object, not the byte after it. The
            // offset can therefore only name the object, so the key of the
            // member it lacks is carried alongside it.
            self.idx = open;
            self.error_key = Fields::<O, T>::missing(seen);
            return Err(ErrorCode::MissingKey);
        }
        Ok(())
    }

    /// The cursor sits just past a key's opening quote and the key is not one
    /// of ours. Discard the key and its value.
    fn skip_unknown_member(&mut self) -> PResult<()> {
        self.skip_string_body()?;
        self.colon()?;
        self.skip_value()
    }

    /// Read a JSON enum into a type declared with `unit_enum!` or
    /// `tagged_enum!`.
    ///
    /// Two forms, told apart by the first byte. A bare `"Name"` is a variant
    /// carrying nothing; a `{"Name":value}` is one carrying a value, and the
    /// object holds that single member and no other. Either way the name is
    /// hashed to a candidate variant, and the generated dispatch confirms it.
    ///
    /// A name no variant claims is an
    /// [`ErrorCode::UnknownVariant`] under every policy, including
    /// [`SkipUnknown`](crate::SkipUnknown). Stepping over an unknown object
    /// key still leaves the object readable; stepping over an unknown variant
    /// would leave the value itself undecided, so there is nothing to fall
    /// back to.
    pub fn read_enum<T: ReadEnum<'de>>(&mut self, value: &mut T) -> PResult<()> {
        self.skip_ws();
        match self.peek() {
            Some(b'"') => {
                self.idx += 1;
                self.dispatch_variant(value, T::read_name)
            }
            Some(b'{') => {
                // Where the object begins, so an object that holds no tag is
                // reported against the object rather than against the brace
                // that closed it.
                let open = self.idx;
                self.idx += 1;
                self.enter()?;
                self.skip_ws();
                // The tag is the object's whole content, so no members names no
                // variant exactly as two do.
                if self.peek() == Some(b'}') {
                    self.idx = open;
                    return Err(ErrorCode::ExpectedVariant);
                }
                self.expect(b'"', ErrorCode::ExpectedQuote)?;
                self.dispatch_variant(value, T::read_payload)?;
                // A comma here is that second member.
                if self.comma_or_close(b'}')? {
                    self.idx = open;
                    return Err(ErrorCode::ExpectedVariant);
                }
                self.leave();
                Ok(())
            }
            // A document that ended is not a document that held the wrong
            // thing, and every other reader here tells the two apart.
            Some(_) => Err(ErrorCode::ExpectedVariant),
            None => Err(ErrorCode::UnexpectedEnd),
        }
    }

    /// The half [`Self::read_enum`]'s two arms share: hash the name at the
    /// cursor, hand the candidate to `take`, and report a name nothing claimed
    /// against the name itself.
    #[inline]
    fn dispatch_variant<T, F>(&mut self, value: &mut T, take: F) -> PResult<()>
    where
        T: ReadEnum<'de>,
        F: FnOnce(&mut T, usize, &mut Self) -> PResult<bool>,
    {
        let map = T::MAP;
        // Where the name begins. `read_name` and `read_payload` consume it
        // only once they have matched it, but they are safe traits and nothing
        // obliges them to, so the position is restored rather than assumed.
        let at = self.idx;
        let index = map.lookup(T::VARIANTS, self.rest());
        if index < map.n as usize && take(value, index, self)? {
            return Ok(());
        }
        // `match_key` fails identically for a name that differs and for one
        // the input ended in the middle of, so reaching here is not yet
        // evidence of a schema mismatch. Walking the name to its closing quote
        // is what tells the two apart, and it reports the truncation itself.
        // This is `read_object`'s step, for `read_object`'s reason.
        self.idx = at;
        self.skip_string_body()?;
        // Back to the first byte of the name, so the position this error
        // carries names what was not recognized.
        self.idx = at;
        Err(ErrorCode::UnknownVariant)
    }

    /// Read a JSON array into a type declared with `array!`.
    ///
    /// Position is the whole schema, so there is no key to hash and none to
    /// confirm: element `i` goes to field `i`, and the only thing to check is
    /// that the document held exactly as many as the struct has.
    #[inline]
    pub fn read_array<T: ReadArray<'de>>(&mut self, value: &mut T) -> PResult<()> {
        let count = self.read_seq(|p, i| value.read_element(i, p))?;
        if count != T::LEN {
            return Err(ErrorCode::ArrayLengthMismatch);
        }
        Ok(())
    }

    /// Drive a JSON array, calling `element` once per entry.
    ///
    /// `element` receives the zero-based position so container implementations
    /// can reuse storage they already hold.
    #[inline]
    pub fn read_seq<F>(&mut self, mut element: F) -> PResult<usize>
    where
        F: FnMut(&mut Self, usize) -> PResult<()>,
    {
        self.skip_ws();
        self.expect(b'[', ErrorCode::ExpectedBracket)?;
        self.enter()?;
        self.skip_ws();

        if self.try_byte(b']') {
            self.leave();
            return Ok(0);
        }

        let mut count = 0usize;
        loop {
            element(self, count)?;
            count += 1;
            if !self.comma_or_close(b']')? {
                self.leave();
                return Ok(count);
            }
        }
    }

    /// Drive a JSON object as a map, calling `entry` with each key.
    ///
    /// The key is passed as a borrowed `&'de str` when it has no escapes, which
    /// is the overwhelmingly common case, so map keys usually cost no
    /// allocation beyond the map's own.
    #[inline]
    pub fn read_map<F>(&mut self, mut entry: F) -> PResult<()>
    where
        F: FnMut(&mut Self, JsonStr<'de>) -> PResult<()>,
    {
        self.skip_ws();
        self.expect(b'{', ErrorCode::ExpectedBrace)?;
        self.enter()?;
        self.skip_ws();

        if self.try_byte(b'}') {
            self.leave();
            return Ok(());
        }

        loop {
            self.expect(b'"', ErrorCode::ExpectedQuote)?;
            let key = self.read_string_body()?;
            self.colon()?;
            entry(self, key)?;
            if !self.comma_or_close(b'}')? {
                self.leave();
                return Ok(());
            }
        }
    }

    // -----------------------------------------------------------------------
    // Scalars
    // -----------------------------------------------------------------------

    #[inline]
    pub fn read_bool(&mut self) -> PResult<bool> {
        match self.peek() {
            Some(b't') => {
                self.expect_lit(b"true", ErrorCode::ExpectedTrue)?;
                Ok(true)
            }
            Some(b'f') => {
                self.expect_lit(b"false", ErrorCode::ExpectedFalse)?;
                Ok(false)
            }
            Some(_) => Err(ErrorCode::UnexpectedCharacter),
            None => Err(ErrorCode::UnexpectedEnd),
        }
    }

    #[inline(always)]
    pub(crate) fn expect_lit(&mut self, lit: &[u8], code: ErrorCode) -> PResult<()> {
        let n = lit.len();
        if self.remaining() >= n && &self.data[self.idx..self.idx + n] == lit {
            self.idx += n;
            Ok(())
        } else {
            Err(code)
        }
    }

    /// Consume `null`, reporting whether it was there. Non-consuming otherwise.
    #[inline(always)]
    pub fn try_null(&mut self) -> PResult<bool> {
        if self.peek() == Some(b'n') {
            self.expect_lit(b"null", ErrorCode::ExpectedNull)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    #[inline]
    pub fn read_u64(&mut self) -> PResult<u64> {
        let v = parse_u64(self.data, &mut self.idx)?;
        reject_float_tail(self.data, self.idx)?;
        Ok(v)
    }

    #[inline]
    pub fn read_i64(&mut self) -> PResult<i64> {
        let v = parse_i64(self.data, &mut self.idx)?;
        reject_float_tail(self.data, self.idx)?;
        Ok(v)
    }

    #[inline]
    pub fn read_f64(&mut self) -> PResult<f64> {
        parse_float::<f64>(self.data, &mut self.idx)
    }

    #[inline]
    pub fn read_f32(&mut self) -> PResult<f32> {
        parse_float::<f32>(self.data, &mut self.idx)
    }

    /// Parse a 128-bit unsigned integer.
    ///
    /// Wide integers are rare, so this is a straightforward digit loop rather
    /// than the SWAR path the 64-bit case uses.
    pub fn read_u128(&mut self) -> PResult<u128> {
        let n = self.data.len();
        let mut i = self.idx;
        if i >= n || !self.data[i].is_ascii_digit() {
            return Err(ErrorCode::ExpectedNumber);
        }
        if self.data[i] == b'0' {
            i += 1;
            if i < n && self.data[i].is_ascii_digit() {
                return Err(ErrorCode::InvalidNumber);
            }
            self.idx = i;
            reject_float_tail(self.data, i)?;
            return Ok(0);
        }
        let mut v: u128 = 0;
        while i < n {
            let c = self.data[i].wrapping_sub(b'0');
            if c >= 10 {
                break;
            }
            v = v
                .checked_mul(10)
                .and_then(|x| x.checked_add(c as u128))
                .ok_or(ErrorCode::NumberOutOfRange)?;
            i += 1;
        }
        self.idx = i;
        reject_float_tail(self.data, i)?;
        Ok(v)
    }

    /// Parse a 128-bit signed integer.
    pub fn read_i128(&mut self) -> PResult<i128> {
        let negative = self.peek() == Some(b'-');
        if negative {
            self.idx += 1;
        }
        let magnitude = self.read_u128()?;
        if negative {
            // `i128::MIN` has no positive counterpart, so compare before
            // negating.
            if magnitude > (i128::MAX as u128) + 1 {
                return Err(ErrorCode::NumberOutOfRange);
            }
            Ok((magnitude as i128).wrapping_neg())
        } else {
            if magnitude > i128::MAX as u128 {
                return Err(ErrorCode::NumberOutOfRange);
            }
            Ok(magnitude as i128)
        }
    }

    /// Borrow a number's text out of the input, without converting it.
    ///
    /// The token is validated against the JSON number grammar and the cursor
    /// is left just past it, exactly as [`read_f64`](Self::read_f64) leaves
    /// it; what comes back is the literal itself, sign and exponent included.
    ///
    /// This is for a scalar none of the conversions above can hold: a
    /// fixed-point or decimal type, an arbitrary-precision integer, a
    /// rational. Reading such a value as an `f64` and converting is not an
    /// implementation of it, since the rounding is the thing the type exists
    /// to avoid; the digits are what the caller needs, so the digits are what
    /// this returns. [`Writer::write_number_str`](crate::json::Writer::write_number_str)
    /// is the other half.
    ///
    /// BEVE has no untyped number, so a type described this way has to pick a
    /// binary form of its own; there is no equivalent on
    /// [`beve::Reader`](crate::beve::Reader) to pair with.
    ///
    /// ```
    /// use structio::{ErrorCode, Options, from_str, json, to_string};
    ///
    /// /// Stands in for a decimal type. What matters is that the digits
    /// /// arrive whole; how one stores them is its own business.
    /// #[derive(Default)]
    /// struct Decimal(String);
    ///
    /// impl<'de> json::Read<'de> for Decimal {
    ///     fn read<O: Options>(&mut self, p: &mut json::Parser<'de, O>) -> Result<(), ErrorCode> {
    ///         self.0.clear();
    ///         self.0.push_str(p.read_number_str()?);
    ///         Ok(())
    ///     }
    /// }
    ///
    /// impl json::Write for Decimal {
    ///     fn write<O: Options>(&self, w: &mut json::Writer<'_, O>) {
    ///         w.write_number_str(&self.0);
    ///     }
    /// }
    ///
    /// // Past an `f64`'s range and past its precision, and rounded by neither.
    /// let text = "-1.2345678901234567890123e400";
    /// let d: Decimal = from_str(text).unwrap();
    /// assert_eq!(d.0, text);
    /// assert_eq!(to_string(&d), text);
    ///
    /// // A token, not a span: what is not a number is refused here rather
    /// // than by whoever parses the digits next.
    /// assert!(from_str::<Decimal>("01").is_err());
    /// ```
    #[inline]
    pub fn read_number_str(&mut self) -> PResult<&'de str> {
        let start = self.idx;
        scan_number(self.data, &mut self.idx)?;
        // SAFETY: the input was a `&str`, and the scanner accepts only ASCII
        // bytes, so both ends of this range are char boundaries and the range
        // is valid UTF-8.
        Ok(unsafe { core::str::from_utf8_unchecked(&self.data[start..self.idx]) })
    }

    // -----------------------------------------------------------------------
    // Strings
    // -----------------------------------------------------------------------

    /// Scan a string body starting at `from` (just past the opening quote).
    ///
    /// `Ok(Ok(text))` is the common case: no escapes, so the text is a subslice
    /// of the document and the cursor has moved past the closing quote.
    /// `Ok(Err(pos))` reports the first escape at `pos` and leaves the cursor
    /// alone, so the caller can decide whether to expand it or refuse.
    ///
    /// The three public string readers differ only in what they do with those
    /// two outcomes, so the scan, the bounds, and the one unchecked UTF-8
    /// conversion all live here.
    #[inline(always)]
    fn scan_body(&mut self, from: usize) -> PResult<::core::result::Result<&'de str, usize>> {
        match scan_string(self.data, from) {
            Some((pos, b'"')) => {
                self.idx = pos + 1;
                // SAFETY: the input was a `&str` and `"` is ASCII, so this
                // range starts and ends on char boundaries and is valid UTF-8.
                Ok(Ok(unsafe {
                    core::str::from_utf8_unchecked(&self.data[from..pos])
                }))
            }
            Some((pos, b'\\')) => Ok(Err(pos)),
            Some(_) => Err(ErrorCode::ControlCharacterInString),
            None => Err(ErrorCode::UnexpectedEnd),
        }
    }

    /// Read a complete JSON string, including its quotes.
    #[inline]
    pub fn read_string(&mut self) -> PResult<JsonStr<'de>> {
        self.expect(b'"', ErrorCode::ExpectedQuote)?;
        self.read_string_body()
    }

    /// Read a string whose opening quote has already been consumed.
    ///
    /// Returns a borrowed slice when there are no escapes. Only a string that
    /// actually contains an escape pays for an allocation.
    #[inline]
    pub fn read_string_body(&mut self) -> PResult<JsonStr<'de>> {
        let start = self.idx;
        match self.scan_body(start)? {
            Ok(s) => Ok(JsonStr::Borrowed(s)),
            Err(first) => {
                let mut out = String::new();
                self.unescape_into(start, first, &mut out)?;
                Ok(JsonStr::Owned(out))
            }
        }
    }

    /// Read a string into an existing `String`, reusing its allocation.
    ///
    /// Reusing the buffer is why repeated reads into the same value do not
    /// allocate, which is the same reason Glaze reads into an existing object
    /// rather than returning a fresh one.
    #[inline]
    pub fn read_string_into(&mut self, out: &mut String) -> PResult<()> {
        self.expect(b'"', ErrorCode::ExpectedQuote)?;
        let start = self.idx;
        match self.scan_body(start)? {
            Ok(s) => {
                out.clear();
                out.push_str(s);
                Ok(())
            }
            Err(first) => {
                out.clear();
                self.unescape_into(start, first, out)
            }
        }
    }

    /// Borrow a string slice directly out of the input, refusing to allocate.
    #[inline]
    pub fn read_str(&mut self) -> PResult<&'de str> {
        self.expect(b'"', ErrorCode::ExpectedQuote)?;
        let start = self.idx;
        match self.scan_body(start)? {
            Ok(s) => Ok(s),
            Err(_) => Err(ErrorCode::EscapeInBorrowedString),
        }
    }

    /// Expand a string containing escapes into `out`.
    ///
    /// `start` is just past the opening quote and `first` is the backslash the
    /// caller's scan already located, so the run between them is copied without
    /// being scanned a second time.
    fn unescape_into(&mut self, start: usize, first: usize, out: &mut String) -> PResult<()> {
        // SAFETY: every byte range appended below is either a slice of the
        // original `&str` delimited by ASCII bytes, or the UTF-8 encoding of a
        // `char`, so `out` stays valid UTF-8 throughout.
        let bytes = unsafe { out.as_mut_vec() };
        bytes.extend_from_slice(&self.data[start..first]);
        let mut i = self.expand_escape(first + 1, bytes)?;
        loop {
            let stop = match scan_string(self.data, i) {
                Some((pos, _)) => pos,
                None => return Err(ErrorCode::UnexpectedEnd),
            };
            bytes.extend_from_slice(&self.data[i..stop]);
            match self.data[stop] {
                b'"' => {
                    self.idx = stop + 1;
                    return Ok(());
                }
                b'\\' => {
                    i = self.expand_escape(stop + 1, bytes)?;
                }
                _ => return Err(ErrorCode::ControlCharacterInString),
            }
        }
    }

    /// Expand one escape starting at `i` (just past the backslash). Returns the
    /// index of the first byte after it.
    fn expand_escape(&self, i: usize, out: &mut Vec<u8>) -> PResult<usize> {
        let c = *self.data.get(i).ok_or(ErrorCode::UnexpectedEnd)?;
        let simple = match c {
            b'"' => b'"',
            b'\\' => b'\\',
            b'/' => b'/',
            b'b' => 0x08,
            b'f' => 0x0C,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'u' => {
                let (ch, next) = self.read_unicode_escape(i + 1)?;
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                return Ok(next);
            }
            _ => return Err(ErrorCode::InvalidEscape),
        };
        out.push(simple);
        Ok(i + 1)
    }

    /// Decode `\uXXXX`, joining a surrogate pair when one is present.
    fn read_unicode_escape(&self, i: usize) -> PResult<(char, usize)> {
        let hi = self.read_hex4(i)?;
        let mut next = i + 4;

        if (0xD800..0xDC00).contains(&hi) {
            // High surrogate: a low surrogate must follow, as its own escape.
            if self.data.get(next) != Some(&b'\\') || self.data.get(next + 1) != Some(&b'u') {
                return Err(ErrorCode::InvalidSurrogate);
            }
            let lo = self.read_hex4(next + 2)?;
            if !(0xDC00..0xE000).contains(&lo) {
                return Err(ErrorCode::InvalidSurrogate);
            }
            next += 6;
            let cp = 0x1_0000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
            let ch = char::from_u32(cp).ok_or(ErrorCode::InvalidSurrogate)?;
            return Ok((ch, next));
        }
        if (0xDC00..0xE000).contains(&hi) {
            // An unpaired low surrogate is not a scalar value.
            return Err(ErrorCode::InvalidSurrogate);
        }
        let ch = char::from_u32(hi).ok_or(ErrorCode::InvalidSurrogate)?;
        Ok((ch, next))
    }

    #[inline]
    fn read_hex4(&self, i: usize) -> PResult<u32> {
        if i + 4 > self.data.len() {
            return Err(ErrorCode::UnexpectedEnd);
        }
        let mut v = 0u32;
        for k in 0..4 {
            let d = match self.data[i + k] {
                c @ b'0'..=b'9' => (c - b'0') as u32,
                c @ b'a'..=b'f' => (c - b'a' + 10) as u32,
                c @ b'A'..=b'F' => (c - b'A' + 10) as u32,
                _ => return Err(ErrorCode::InvalidEscape),
            };
            v = (v << 4) | d;
        }
        Ok(v)
    }

    /// Step past a string body whose opening quote is already consumed,
    /// without materializing it.
    ///
    /// One SWAR pass finds the closing quote, and finds a backslash or a
    /// control character on the way for free, so the escape is stepped over and
    /// the control character refused at no extra cost. The cursor stays on the
    /// body when this fails, so the error names the string rather than wherever
    /// the scan gave up inside it.
    fn skip_string_body(&mut self) -> PResult<()> {
        let mut i = self.idx;
        loop {
            match scan_string(self.data, i) {
                Some((pos, b'"')) => {
                    self.idx = pos + 1;
                    return Ok(());
                }
                Some((pos, b'\\')) => {
                    // Step over the backslash and whatever it escapes, so an
                    // escaped quote does not end the scan.
                    i = pos + 2;
                    if i > self.data.len() {
                        return Err(ErrorCode::UnexpectedEnd);
                    }
                }
                Some(_) => return Err(ErrorCode::ControlCharacterInString),
                None => return Err(ErrorCode::UnexpectedEnd),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Skipping
    // -----------------------------------------------------------------------

    /// Discard the next value, whatever it is.
    pub fn skip_value(&mut self) -> PResult<()> {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => {
                self.idx += 1;
                self.enter()?;
                self.skip_ws();
                if self.try_byte(b'}') {
                    self.leave();
                    return Ok(());
                }
                loop {
                    self.expect(b'"', ErrorCode::ExpectedQuote)?;
                    self.skip_string_body()?;
                    self.colon()?;
                    self.skip_value()?;
                    if !self.comma_or_close(b'}')? {
                        self.leave();
                        return Ok(());
                    }
                }
            }
            Some(b'[') => {
                self.idx += 1;
                self.enter()?;
                self.skip_ws();
                if self.try_byte(b']') {
                    self.leave();
                    return Ok(());
                }
                loop {
                    self.skip_value()?;
                    if !self.comma_or_close(b']')? {
                        self.leave();
                        return Ok(());
                    }
                }
            }
            // Everything that is not a container is a scalar, and there is one
            // skipper for those; the whitespace it assumes away is behind the
            // cursor already.
            _ => self.skip_scalar(),
        }
    }

    /// Step over a scalar already at the cursor: a string, a number, or one of
    /// the three literals.
    ///
    /// [`skip_value`](Self::skip_value) with the containers taken out and the
    /// leading whitespace assumed away, for a caller that has dispatched on the
    /// byte itself and skipped the whitespace to find it. `skip_value` is that
    /// caller once it has ruled out a container, so there is one scalar skipper
    /// rather than one per walk.
    ///
    /// A number is stepped over by its alphabet rather than held to the
    /// grammar. Nothing here reads its value, and the two callers both have
    /// somewhere better for a malformed one to be caught: a skipped value is
    /// discarded, and a copied one is republished for whoever reads it next to
    /// reject. See [`prettify`](crate::prettify).
    #[inline]
    pub(crate) fn skip_scalar(&mut self) -> PResult<()> {
        match self.peek() {
            Some(b'"') => {
                self.idx += 1;
                self.skip_string_body()
            }
            Some(b't') => self.expect_lit(b"true", ErrorCode::ExpectedTrue),
            Some(b'f') => self.expect_lit(b"false", ErrorCode::ExpectedFalse),
            Some(b'n') => self.expect_lit(b"null", ErrorCode::ExpectedNull),
            Some(c) if c == b'-' || c.is_ascii_digit() => {
                let mut i = self.idx;
                let n = self.data.len();
                while i < n {
                    match self.data[i] {
                        b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E' => i += 1,
                        _ => break,
                    }
                }
                // The guard above admits only bytes the loop accepts, so at
                // least one was taken and the token is never empty.
                self.idx = i;
                Ok(())
            }
            None => Err(ErrorCode::UnexpectedEnd),
            Some(_) => Err(ErrorCode::UnexpectedCharacter),
        }
    }

    /// After the top-level value: only trailing whitespace is allowed.
    #[inline]
    pub fn finish(&mut self) -> PResult<()> {
        self.skip_ws();
        if self.idx == self.data.len() {
            Ok(())
        } else {
            Err(ErrorCode::TrailingContent)
        }
    }

    /// Read any value into `T`.
    #[inline(always)]
    pub fn read<T: Read<'de>>(&mut self, value: &mut T) -> PResult<()> {
        value.read(self)
    }
}

/// The four bytes JSON calls whitespace.
///
/// The one definition of it in the crate. The reader and the
/// [minifier](crate::minify()) share a walk over runs of it, in
/// [`skip_ws_at`]; the stream splitter has its own, because its input grows
/// under it and a run can end in the middle of nothing. What none of them may
/// do is disagree about what whitespace is.
#[inline(always)]
pub(crate) const fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

/// Could this byte be part of a number or one of the three literals?
///
/// The alphabet those are spelled from, and so the answer to "would these two
/// tokens run together": `1` beside `2` is `12`, and `true` beside `false` is
/// one long word, while punctuation and a quote delimit themselves. The stream
/// splitter uses it to find where a bare top-level value ends, and the
/// [minifier](crate::minify()) to know which whitespace it must not remove.
///
/// Deliberately generous. Neither caller is deciding whether a token is spelled
/// properly, only where it stops; the real parser makes that judgement when the
/// span reaches it.
#[inline(always)]
pub(crate) const fn scalar_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'-' | b'+' | b'.')
}

/// First byte at or after `at` that is not whitespace.
///
/// The body of [`Parser::skip_ws`], reachable without a parser so that the
/// [minifier](crate::minify()), which walks its input by index rather than by
/// cursor, draws the line between whitespace and a token exactly where the
/// reader draws it.
#[inline(always)]
pub(crate) fn skip_ws_at<O: Options>(data: &[u8], at: usize) -> usize {
    let mut i = at;
    while i < data.len() {
        match data[i] {
            c if is_ws(c) => i += 1,
            b'/' if O::ALLOW_COMMENTS => match skip_comment(data, i) {
                Some(after) => i = after,
                None => break,
            },
            _ => break,
        }
    }
    i
}

/// Step over one comment, `data[at]` being its `/`.
///
/// `data` is the whole of the input, which is what lets a `//` running to the
/// end of it be a comment that ended: there is no newline to come. The
/// streaming splitter, whose input grows, has to answer that differently.
///
/// `Some(end)` is the first byte after it: for `//` the newline that ended it,
/// which the whitespace loop takes next, and for `/* */` the byte past the
/// closing slash. `None` means there is no complete comment here, either
/// because the `/` begins nothing or because a block comment was never closed,
/// and the cursor is left on the `/` so that the error names it.
///
/// Out of line so that [`Parser::skip_ws`], which is inlined at every token
/// boundary, stays the small loop it was.
#[inline]
pub(crate) fn skip_comment(data: &[u8], at: usize) -> Option<usize> {
    let body = at + 2;
    match data.get(at + 1)? {
        b'/' => Some(find_byte(data, body, b'\n').unwrap_or(data.len())),
        // A pair at a time. A comment is a rare, short thing next to the
        // document around it, and a two-byte terminator is not what the
        // word-at-a-time search in `swar` is shaped for.
        b'*' => {
            let end = data.get(body..)?.windows(2).position(|w| w == b"*/")?;
            Some(body + end + 2)
        }
        _ => None,
    }
}

/// A string read from the input, borrowed when it contained no escapes.
///
/// Deliberately not `Cow`: the borrowed case is the overwhelmingly common one,
/// and giving it a type of our own keeps that visible at every call site.
pub enum JsonStr<'de> {
    Borrowed(&'de str),
    Owned(String),
}

impl<'de> JsonStr<'de> {
    #[inline(always)]
    pub fn as_str(&self) -> &str {
        match self {
            JsonStr::Borrowed(s) => s,
            JsonStr::Owned(s) => s,
        }
    }

    #[inline]
    pub fn into_string(self) -> String {
        match self {
            JsonStr::Borrowed(s) => s.to_owned(),
            JsonStr::Owned(s) => s,
        }
    }
}

/// First byte at or after `from` that ends or complicates a string: `"`, `\`,
/// or a control character. Returns it with its index.
///
/// One SWAR pass tests all three conditions over eight bytes at a time.
#[inline(always)]
fn scan_string(data: &[u8], from: usize) -> Option<(usize, u8)> {
    let n = data.len();
    let mut i = from;
    while i + 8 <= n {
        // SAFETY: `i + 8 <= n`, so the eight bytes read are in bounds.
        let m = escape_mask(unsafe { load_u64(data, i) });
        if m != 0 {
            let pos = i + first_match(m);
            return Some((pos, data[pos]));
        }
        i += 8;
    }
    while i < n {
        let c = data[i];
        if needs_escape(c) {
            return Some((i, c));
        }
        i += 1;
    }
    None
}
