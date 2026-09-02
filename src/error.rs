//! Error types.
//!
//! Hot paths propagate a bare [`ErrorCode`], which is a single byte, so
//! `Result<(), ErrorCode>` is register sized and `?` costs a test-and-branch.
//! The byte offset is attached once, at the public entry point, from the
//! cursor position at the moment the parse stopped.

use core::fmt::{self, Write as _};

/// What went wrong. One byte, so error propagation stays cheap.
///
/// One set covers both formats, and a code does not say which one produced it.
/// A few belong to one format by construction, [`ExpectedBrace`](Self::ExpectedBrace)
/// to JSON and [`InvalidHeader`](Self::InvalidHeader) to BEVE, but most of the
/// set is shared and which ones are is not a promise. The entry point is what
/// names the format, so code that needs to report which codec failed should
/// record it where it chose the codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum ErrorCode {
    // Structural
    UnexpectedEnd,
    ExpectedBrace,
    ExpectedBracket,
    ExpectedQuote,
    ExpectedColon,
    ExpectedComma,
    ExpectedTrue,
    ExpectedFalse,
    ExpectedNull,
    UnexpectedCharacter,
    TrailingContent,
    ExceededMaxDepth,
    /// A streaming reader would have had to buffer more than its configured
    /// limit to hold one value. See [`Documents::max_value`].
    ///
    /// [`Documents::max_value`]: crate::Documents::max_value
    DocumentTooLarge,

    // BEVE structure
    /// A header byte named a type, width, or extension this crate cannot read.
    InvalidHeader,
    /// A well-formed BEVE construct with nowhere to go: a 128-bit float, an
    /// extension beyond the four the specification defines, or, when
    /// [transcoding](crate::transcode), any extension at all.
    UnsupportedFeature,
    /// An object's keys were of a kind the destination cannot take, such as
    /// integer keys for a struct.
    UnsupportedKeyType,

    // Type mismatches
    ExpectedObject,
    ExpectedArray,
    ExpectedString,
    ExpectedBool,
    ExpectedInteger,
    /// An array of one-byte elements was expected, for a borrowed `&[u8]`.
    ExpectedBytes,
    /// A typed array's stored element type was not the one a bulk read needs.
    ///
    /// Reading is otherwise lenient about width: an `i64` field takes a stored
    /// `i16` and a `f64` takes a stored `f32`, because the value fits. That
    /// leniency is a conversion done one element at a time, so the paths that
    /// exist to move a whole block at once cannot offer it, and say this
    /// instead of quietly becoming the slow path. See
    /// [`read_beve_array_into`](crate::read_beve_array_into).
    ElementTypeMismatch,
    /// A [`Complex`](crate::Complex) was expected but the value was neither a
    /// complex extension nor a two-element array.
    ExpectedComplex,
    /// A BEVE value was read as a [`Matrix`](crate::Matrix) but was neither a
    /// matrix extension nor an object. BEVE only: the JSON reader wants an
    /// object like any other and says [`ExpectedBrace`](Self::ExpectedBrace).
    ExpectedMatrix,
    /// An enum was neither a variant name nor an object holding exactly one
    /// member that names one.
    ///
    /// The two forms are the whole encoding: a variant carrying nothing is its
    /// name, and a variant carrying a value is that name used as the single
    /// key of an object. Anything else, an object with no members or with two,
    /// a number, an array, is not a variant at all.
    ExpectedVariant,
    /// An internally tagged enum's object did not begin with its tag.
    ///
    /// [`internally_tagged_enum!`](crate::internally_tagged_enum) reads in one
    /// pass, so the member deciding which variant this is has to arrive before
    /// the members whose meaning it decides. An object whose first member is
    /// some other key is refused here rather than searched: finding a later tag
    /// would mean holding the object somewhere or walking it twice, and this
    /// crate does neither.
    ///
    /// The same code covers an object with no members at all, and a tag whose
    /// value is not a string. Which of the three it was is not distinguished,
    /// because distinguishing them is the search being refused. The reported
    /// position is the object's first member, or the object itself when it has
    /// none.
    ///
    /// A tag that *is* first and names nothing is
    /// [`UnknownVariant`](Self::UnknownVariant): the tag was found, and its
    /// value is not a variant.
    ExpectedTag,

    // Values
    NumberOutOfRange,
    InvalidNumber,
    ExpectedNumber,
    InvalidEscape,
    InvalidSurrogate,
    InvalidUtf8,
    ControlCharacterInString,
    /// A borrowed `&str` was requested but the JSON string contained escapes,
    /// so no subslice of the input can represent it.
    EscapeInBorrowedString,
    /// A fixed-size target (`[T; N]`, a tuple) did not match the JSON length.
    ArrayLengthMismatch,
    /// A `char` was requested but the string was not exactly one scalar value.
    ExpectedSingleChar,
    /// An object held a key that no field of the destination claims, under the
    /// default [`Options::ERROR_ON_UNKNOWN_KEYS`](crate::Options::ERROR_ON_UNKNOWN_KEYS).
    ///
    /// Read with [`SkipUnknown`](crate::SkipUnknown) to step over it instead.
    ///
    /// For a struct declared with [`object!`](crate::object) the reported
    /// position is the key itself, so the message names what was not
    /// recognized. A hand-written reader reports wherever it noticed, which
    /// for [`Matrix`](crate::Matrix) is the offending member's value.
    UnknownKey,
    /// An enum's tag named no variant the destination declares.
    ///
    /// Unlike [`UnknownKey`](Self::UnknownKey) this is refused under every
    /// policy, [`SkipUnknown`](crate::SkipUnknown) included. A member with
    /// nowhere to go can be stepped over and the rest of the object still
    /// read; a variant with nowhere to go leaves the value itself undecided.
    UnknownVariant,
    /// An object left out a field that had to be there: one marked
    /// `#[required]` in the declaration, or any of them under
    /// [`Options::ERROR_ON_MISSING_KEYS`](crate::Options::ERROR_ON_MISSING_KEYS).
    ///
    /// Neither is on by default: absence otherwise means the destination keeps
    /// what it already held. Mark the members a document has to carry, or read
    /// with [`RequireKeys`](crate::RequireKeys) to insist on every one.
    ///
    /// Which field is missing is not carried, an [`ErrorCode`] being one byte.
    /// The reported position is where the object began, its opening brace in
    /// JSON and its header byte in BEVE, so the message names the incomplete
    /// object rather than the byte that closed it. [`Matrix`](crate::Matrix)
    /// reads by hand and reports the same way.
    MissingKey,
    /// A matrix held a different number of elements than its extents describe.
    InvalidMatrixShape,
    /// A matrix named a storage order that is not one of the two defined. It
    /// says which index varies fastest, so reading it wrongly would transpose
    /// the data without any length being wrong, which is why it is refused
    /// rather than guessed at.
    InvalidMatrixLayout,

    // Pointers
    /// A pointer was not valid [RFC 6901](https://www.rfc-editor.org/rfc/rfc6901)
    /// syntax: it did not start with `/`, or a token held a stray `~`, or an
    /// array index was not a decimal number without leading zeros. The `-` the
    /// RFC defines for the position after the last element is well formed and
    /// so is [`NoSuchValue`](Self::NoSuchValue) instead.
    InvalidPointer,
    /// A pointer was well formed but named a member or element the document
    /// does not hold.
    NoSuchValue,
}

impl ErrorCode {
    /// A short, stable, human readable description.
    pub const fn message(self) -> &'static str {
        use ErrorCode::*;
        match self {
            UnexpectedEnd => "unexpected end of input",
            ExpectedBrace => "expected '{'",
            ExpectedBracket => "expected '['",
            ExpectedQuote => "expected '\"'",
            ExpectedColon => "expected ':'",
            ExpectedComma => "expected ','",
            ExpectedTrue => "expected 'true'",
            ExpectedFalse => "expected 'false'",
            ExpectedNull => "expected 'null'",
            UnexpectedCharacter => "unexpected character",
            TrailingContent => "trailing content after value",
            ExceededMaxDepth => "exceeded maximum nesting depth",
            DocumentTooLarge => "value exceeds the streaming size limit",
            InvalidHeader => "invalid BEVE header",
            UnsupportedFeature => "unsupported BEVE feature",
            UnsupportedKeyType => "unsupported object key type",
            ExpectedObject => "expected an object",
            ExpectedArray => "expected an array",
            ExpectedString => "expected a string",
            ExpectedBool => "expected a boolean",
            ExpectedInteger => "expected an integer",
            ExpectedBytes => "expected an array of bytes",
            ElementTypeMismatch => "array element type does not match the target",
            ExpectedComplex => "expected a complex number",
            ExpectedMatrix => "expected a matrix",
            ExpectedVariant => "expected an enum variant",
            ExpectedTag => "expected the tag as the object's first member",
            NumberOutOfRange => "number out of range for target type",
            InvalidNumber => "invalid number",
            ExpectedNumber => "expected a number",
            InvalidEscape => "invalid escape sequence",
            InvalidSurrogate => "invalid surrogate pair",
            InvalidUtf8 => "invalid UTF-8",
            ControlCharacterInString => "unescaped control character in string",
            EscapeInBorrowedString => "cannot borrow a string containing escapes",
            ArrayLengthMismatch => "array length does not match target",
            ExpectedSingleChar => "expected a single character",
            UnknownKey => "unknown object key",
            UnknownVariant => "unknown enum variant",
            MissingKey => "missing object key",
            InvalidMatrixShape => "matrix extents do not describe its data",
            InvalidMatrixLayout => "unknown matrix layout",
            InvalidPointer => "invalid JSON Pointer",
            NoSuchValue => "no value at that pointer",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

/// A code stands on its own wherever there is no input to locate it in, which
/// is why [`Matrix::new`](crate::Matrix::new) hands one back rather than an
/// [`Error`]: a value assembled in memory has no byte offset to report.
impl std::error::Error for ErrorCode {}

/// A parse or serialization failure, located within the input.
///
/// The location is an *offset*, not a copy of anything: it indexes the buffer
/// you handed the parser, and it means nothing once that buffer is gone. A
/// caller that converts this into its own error type and drops the input
/// should render it first, with [`display_with`](Self::display_with), and
/// carry the `String`. See [`docs/errors.md`] for the shape that has.
///
/// [`docs/errors.md`]: https://github.com/stephenberry/structio/blob/main/docs/errors.md
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error {
    /// What went wrong.
    pub code: ErrorCode,
    /// Byte offset into the input where the failure was detected.
    pub index: usize,
    /// The key the code is about, where the crate knows one, and `None` where
    /// it does not.
    ///
    /// The offset answers "where", and for a text document read next to
    /// [`display_with`](Self::display_with) that is usually the whole answer.
    /// It is a poor answer for a member that is *not in the document*, which
    /// has no position of its own, and a thin answer for BEVE, a byte offset
    /// into a binary document being nearly unreadable. So the one code that
    /// fills this in is [`MissingKey`](ErrorCode::MissingKey), whose offset is
    /// the enclosing object's first byte and whose name is the absent key.
    ///
    /// Every other code leaves it empty, [`UnknownKey`](ErrorCode::UnknownKey)
    /// and [`UnknownVariant`](ErrorCode::UnknownVariant) included: the cursor
    /// is already wound back to the offending name, so the offset points at it
    /// and a copy here would say the same thing twice.
    ///
    /// `&'static str`, so an `Error` still outlives the document and stays
    /// `Copy`. Everything nameable here is a constant of the destination type
    /// rather than a run of input bytes, which is what makes that possible.
    ///
    /// [`Option`] rather than an empty string, which would cost nothing here
    /// -- the two are the same 32 bytes, the reference being niche-packed --
    /// and would conflate two different answers. An empty key is legal JSON,
    /// so `#[required] "" => field` is a schema whose missing key really is
    /// named `""`, and `Some("")` says that where `""` alone could not.
    ///
    /// A hand-written reader sets it with [`json::Parser::set_error_key`] or
    /// [`beve::Reader::set_error_key`].
    ///
    /// ```
    /// use structio::ErrorCode;
    ///
    /// #[derive(Debug, Default)]
    /// struct Accessor { byte_offset: u32, component_type: u32 }
    ///
    /// structio::object!(Accessor as "camelCase" {
    ///     #[required] byte_offset,
    ///     #[required] "type" => component_type,
    /// });
    ///
    /// let e = structio::from_str::<Accessor>(r#"{"type":5}"#).unwrap_err();
    /// assert_eq!(e.code, ErrorCode::MissingKey);
    /// // The key the document uses, not the Rust field.
    /// assert_eq!(e.key, Some("byteOffset"));
    /// // And the offset is the object, which is all an absent member has.
    /// assert_eq!(e.to_string(), r#"missing object key "byteOffset" at byte 0"#);
    /// ```
    ///
    /// [`json::Parser::set_error_key`]: crate::json::Parser::set_error_key
    /// [`beve::Reader::set_error_key`]: crate::beve::Reader::set_error_key
    pub key: Option<&'static str>,
}

impl Error {
    /// A failure at an offset, about no key in particular.
    #[inline]
    pub const fn new(code: ErrorCode, index: usize) -> Self {
        Self {
            code,
            index,
            key: None,
        }
    }

    /// A failure at an offset, about a key the reader named.
    ///
    /// [`Option`] rather than `&'static str`, because this is what an entry
    /// point composes with [`json::Parser::error_key`] and its BEVE twin,
    /// which have a key only sometimes. See [`key`](Self::key).
    ///
    /// [`json::Parser::error_key`]: crate::json::Parser::error_key
    #[inline]
    pub const fn with_key(code: ErrorCode, index: usize, key: Option<&'static str>) -> Self {
        Self { code, index, key }
    }

    /// What went wrong and what it was about, which is the half of a message
    /// that does not depend on where in the document it happened.
    ///
    /// Shared, so [`Display`](fmt::Display) and
    /// [`display_with`](Self::display_with) cannot come to describe the same
    /// error differently. `{:?}` quotes the key, which marks where it begins
    /// and ends.
    ///
    /// `dyn` rather than a generic: this is a diagnostic, so one indirect call
    /// is beneath notice and one copy of the code is worth having.
    fn described(&self, f: &mut dyn fmt::Write) -> fmt::Result {
        f.write_str(self.code.message())?;
        match self.key {
            None => Ok(()),
            Some(key) => write!(f, " {key:?}"),
        }
    }

    /// Render the failure with the surrounding input, for diagnostics.
    ///
    /// Shows the line and column plus a caret under the offending byte, and
    /// the [`key`](Self::key) where there is one.
    ///
    /// The input is the one the offset indexes. Hand it a different document
    /// and you get a caret under an unrelated byte, which is why this is worth
    /// calling at the parse site rather than remembering the offset for later.
    pub fn display_with(&self, input: &str) -> String {
        // `input` is caller-supplied and need not be the document that produced
        // this error, so the index may land inside a character. A diagnostic
        // helper must not panic; round down to the nearest boundary.
        let idx = floor_char_boundary(input, self.index.min(input.len()));
        // Walk to the start of the offending line.
        let line_start = input[..idx].rfind('\n').map_or(0, |p| p + 1);
        let line_end = input[idx..].find('\n').map_or(input.len(), |p| idx + p);
        let line_no = input[..line_start].bytes().filter(|&b| b == b'\n').count() + 1;
        let col_no = input[line_start..idx].chars().count() + 1;

        let line = &input[line_start..line_end];
        // Trim very long lines around the caret so the output stays readable.
        const WINDOW: usize = 80;
        let rel = idx - line_start;
        let (shown, caret_col, elided_left) = if line.len() > WINDOW {
            let lo = rel.saturating_sub(WINDOW / 2);
            let lo = floor_char_boundary(line, lo);
            let hi = ceil_char_boundary(line, (lo + WINDOW).min(line.len()));
            (&line[lo..hi], rel - lo, lo > 0)
        } else {
            (line, rel, false)
        };

        // The caret is drawn in characters, so convert the byte offset the
        // slicing produced; otherwise it drifts right on any line holding
        // multi-byte text, and disagrees with the column just reported.
        let caret_col = shown[..caret_col].chars().count();

        let mut out = String::new();
        // `write!` into a `String` cannot fail, and a diagnostic helper is the
        // last place to propagate an error from.
        let _ = self.described(&mut out);
        let _ = writeln!(out, " at line {line_no}, column {col_no}");
        if elided_left {
            out.push_str("...");
        }
        out.push_str(shown);
        out.push('\n');
        if elided_left {
            out.push_str("   ");
        }
        for _ in 0..caret_col {
            out.push(' ');
        }
        out.push('^');
        out
    }
}

// `str::floor_char_boundary` is still unstable, so roll the two we need.
fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.described(f)?;
        write!(f, " at byte {}", self.index)
    }
}

impl std::error::Error for Error {}

/// Result of a public `structio` operation.
pub type Result<T> = core::result::Result<T, Error>;

/// Result used inside the parser, where the location is implied by the cursor.
pub(crate) type PResult<T> = core::result::Result<T, ErrorCode>;

// ---------------------------------------------------------------------------
// Failures that involve the outside world
// ---------------------------------------------------------------------------

/// A failure from an operation that touches an [`io::Read`] or [`io::Write`].
///
/// Reading through a reader, or writing through a sink, has one failure mode
/// the in-memory API does not: the source or sink itself. [`Error`] is
/// `Copy + Eq` and [`std::io::Error`] is neither, so the I/O case gets its own
/// variant here rather than widening it.
///
/// [`io::Read`]: std::io::Read
/// [`io::Write`]: std::io::Write
#[derive(Debug)]
#[non_exhaustive]
pub enum StreamError {
    /// The underlying reader or writer failed.
    Io(std::io::Error),
    /// The bytes were not what was expected.
    Parse(Error),
}

/// Result of an operation that can fail on I/O as well as on content.
pub type StreamResult<T> = core::result::Result<T, StreamError>;

impl StreamError {
    /// The parse failure, if this was one.
    pub fn as_parse(&self) -> Option<&Error> {
        match self {
            StreamError::Parse(e) => Some(e),
            StreamError::Io(_) => None,
        }
    }

    /// The I/O failure, if this was one.
    pub fn as_io(&self) -> Option<&std::io::Error> {
        match self {
            StreamError::Io(e) => Some(e),
            StreamError::Parse(_) => None,
        }
    }
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamError::Io(e) => write!(f, "i/o error: {e}"),
            StreamError::Parse(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for StreamError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StreamError::Io(e) => Some(e),
            StreamError::Parse(e) => Some(e),
        }
    }
}

impl From<std::io::Error> for StreamError {
    fn from(e: std::io::Error) -> Self {
        StreamError::Io(e)
    }
}

impl From<Error> for StreamError {
    fn from(e: Error) -> Self {
        StreamError::Parse(e)
    }
}
