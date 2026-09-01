//! Compile-time options, for reading and for writing.
//!
//! Options are a *type*, not a value. [`Options`] carries one associated
//! constant per setting, a policy type implements it, and the parsers and
//! writers are generic over that type. Every setting is therefore a `const`
//! inside the code that consults it, so the branch folds away before the
//! optimizer ever sees it and the unselected behaviour costs no code at all.
//!
//! This is the shape Glaze's `glz::opts` has, arrived at differently. Rust has
//! no const generic parameter of struct type on stable, so `write<opts{...}>`
//! has no direct translation; a trait with defaulted associated constants
//! gets the same zero-cost dispatch, and gets a place to document each
//! setting besides.
//!
//! ```
//! use structio::{Pretty, to_string, to_string_with};
//!
//! # #[derive(Default)]
//! # struct P { x: i32 }
//! # structio::object!(P { x });
//! let p = P { x: 1 };
//! assert_eq!(to_string(&p), "{\"x\":1}");
//! assert_eq!(to_string_with::<Pretty, _>(&p), "{\n  \"x\": 1\n}");
//! ```
//!
//! # Writing your own
//!
//! The built-in policies below are the common cases. Any combination is a unit
//! struct and an impl that overrides the constants it cares about; everything
//! left out keeps its default, so a policy written today keeps compiling when
//! a later release adds a setting.
//!
//! ```
//! use structio::Options;
//!
//! /// Indented four spaces, with absent members left out, and tolerant of a
//! /// document written against a newer version of the schema.
//! #[derive(Clone, Copy)]
//! pub struct Config;
//!
//! impl Options for Config {
//!     const PRETTY: bool = true;
//!     const INDENT: usize = 4;
//!     const SKIP_NULL: bool = true;
//!     const ERROR_ON_UNKNOWN_KEYS: bool = false;
//! }
//! ```
//!
//! # What it costs
//!
//! Code size, in exchange for speed: the read and write paths are compiled
//! once per policy a program actually uses. One policy costs exactly what no
//! policy parameter cost before it.
//!
//! # Reading and writing
//!
//! One trait covers both directions, so a policy names everything a program
//! does with a document rather than making you carry two. A setting that
//! belongs to one direction is simply ignored by the other:
//! [`PRETTY`](Options::PRETTY) means nothing to a parser, and
//! [`ERROR_ON_UNKNOWN_KEYS`](Options::ERROR_ON_UNKNOWN_KEYS) means nothing to
//! a writer. Every writing entry point has a `_with` twin and so does every
//! reading one; [`json::Documents`](crate::json::Documents) and
//! [`json::Feed`](crate::json::Feed) take theirs from `with_options`, one more
//! link in the builder chain they already have.

/// How a value is read and written.
///
/// Implemented by [`Standard`], [`Pretty`], [`PrettyInlineArrays`],
/// [`SkipNull`], [`SkipUnknown`], [`RequireKeys`] and [`AllowComments`], and
/// by any policy type of your own.
/// Every constant has a default, so an implementation states only what it
/// changes.
///
/// The trait is a marker: no method, no value, and the implementing type is
/// never constructed. It is named in the writer's type
/// ([`json::Writer<'_, O>`](crate::json::Writer)) and read through
/// `O::CONSTANT` at the point of use.
pub trait Options: Copy {
    /// Write JSON across multiple lines, indented by nesting depth.
    ///
    /// A member's colon gains a trailing space, each member goes on its own
    /// line and so does each element unless
    /// [`NEW_LINES_IN_ARRAYS`](Options::NEW_LINES_IN_ARRAYS) is off, and a
    /// container's closing bracket lines up with the line its opening bracket
    /// began. An empty container stays on one line as `{}` or `[]`, having
    /// nothing to indent.
    ///
    /// BEVE ignores this: a binary document has no whitespace to put anywhere.
    const PRETTY: bool = false;

    /// Spaces per level of nesting under [`PRETTY`](Options::PRETTY).
    ///
    /// Ignored entirely when `PRETTY` is false.
    const INDENT: usize = 2;

    /// Give each element of an array a line of its own under
    /// [`PRETTY`](Options::PRETTY).
    ///
    /// On by default, that being what indenting a document usually means.
    /// Turn it off with [`PrettyInlineArrays`] for documents that are mostly
    /// numbers: a hundred samples in a `Vec<f64>` is a hundred lines holding
    /// one number each, where `[3.0, 4.0, 5.0]` says the same thing on one
    /// line and says it better.
    ///
    /// Off, an array writes as `[a, b, c]`: no break after the opening
    /// bracket or before the closing one, a space after each comma in their
    /// place, and no level of indentation, since nothing is indented against
    /// it. Objects are untouched, the ones inside an array included, so an
    /// array of records opens as `[{` and the members below it are indented
    /// one level from the line the array began on.
    ///
    /// Ignored when `PRETTY` is false, where nothing has a line of its own,
    /// and by BEVE, which has no whitespace to place at all.
    const NEW_LINES_IN_ARRAYS: bool = true;

    /// Leave out object members that hold nothing.
    ///
    /// That means `None`, `()`, and any wrapper around them: a
    /// `Box<Option<T>>` holding `None` is as absent as a bare `None`. A field
    /// left out this way is simply missing from the document, which reading
    /// treats as "keep what the destination already had", so a round trip
    /// through `Default::default()` gets the `None` back.
    ///
    /// Absence is the test, not the spelling of the output. A `f64` holding
    /// NaN writes as `null`, JSON having no other form for it, but it is a
    /// number that is present and it stays. Skipping it would also disagree
    /// with BEVE, which stores the NaN itself.
    ///
    /// **Struct members only.** A `None` inside a sequence still writes
    /// `null`, because dropping it would shorten the sequence and change what
    /// every later index means. A map's entries are likewise left alone: a
    /// null value there is data rather than an absent field, and a map's
    /// length is not known until it has been walked.
    ///
    /// Both formats honour this. BEVE pays slightly more for it than JSON,
    /// since an object states its member count up front and the count is no
    /// longer a compile-time constant once members can drop out.
    const SKIP_NULL: bool = false;

    /// Refuse a document that holds a key no field claims.
    ///
    /// On by default, which is Glaze's default too. A key that no field
    /// claims is far more often a typo, a version skew, or the wrong document
    /// entirely than it is something to pass over in silence, and silence is
    /// the one response that cannot be recovered from: the value lands nowhere
    /// and nothing says so. [`ErrorCode::UnknownKey`](crate::ErrorCode::UnknownKey)
    /// says so.
    ///
    /// Turn it off with [`SkipUnknown`] to read a subset of a larger document,
    /// or to accept one written by a newer version of a schema.
    ///
    /// **Object keys only.** An `array!` struct has no keys, and a length that
    /// does not match is already an error. A map claims every key it is given
    /// by definition. An enum's tag is not a key either, and turning this off
    /// does not make an unrecognized one acceptable: a member with nowhere to
    /// go can be stepped over and the object still read, but a variant with
    /// nowhere to go leaves the value itself undecided, so an
    /// [`ErrorCode::UnknownVariant`](crate::ErrorCode::UnknownVariant) stands
    /// under every policy.
    ///
    /// Both formats honour this, and having it on is cheaper than having it
    /// off: refusing a key costs one branch, where stepping over its value
    /// costs a walk proportional to the size of the value, and a value under
    /// an unknown key can be arbitrarily large. It is also the reading that
    /// looks at strictly fewer bytes, which is what makes it the safer default
    /// as well as the faster one.
    const ERROR_ON_UNKNOWN_KEYS: bool = true;

    /// Refuse a document that leaves a declared field out.
    ///
    /// Off by default, where
    /// [`ERROR_ON_UNKNOWN_KEYS`](Options::ERROR_ON_UNKNOWN_KEYS) is on, and
    /// the asymmetry is deliberate. Reading is into a value that already
    /// exists, so a member the document does not mention means "keep what is
    /// there" rather than "no data": that is what makes
    /// [`read_into`](crate::read_into) a merge, and a default is a
    /// perfectly good answer for a field a document had no opinion about.
    /// Turning this on says the opposite, that the document is the whole
    /// truth about the value, which is right for a wire format and wrong for
    /// a patch.
    ///
    /// Turn it on with [`RequireKeys`]. What it refuses is an
    /// [`ErrorCode::MissingKey`](crate::ErrorCode::MissingKey).
    ///
    /// **All or nothing, which most schemas are not.** A format with a
    /// specification usually has mandatory members and optional ones in the
    /// same object, and neither setting of this fits: off accepts a document
    /// missing something mandatory, on refuses a valid document that left an
    /// optional member out. Mark the mandatory ones `#[required]` in the
    /// declaration instead, which is [`Keys::REQUIRED`](crate::Keys::REQUIRED),
    /// and leave this alone. The two are a union, so a policy that requires
    /// everything still does.
    ///
    /// **Object keys only**, for the same reasons the unknown-key setting is:
    /// an `array!` struct is checked by length already, and a map has no
    /// declared members to miss.
    ///
    /// A field of type `Option<T>` is not exempt. The test is whether the
    /// member is *present*, not what it holds, so `null` satisfies it and
    /// absence does not. Writing under [`SKIP_NULL`](Options::SKIP_NULL) and
    /// reading under this therefore contradict each other by construction:
    /// the writer drops exactly the members the reader insists on.
    ///
    /// Both formats honour this. It costs one `or` per member the schema
    /// claimed and one comparison per object, against a bitmask that never
    /// leaves a register, and nothing at all when it is off.
    ///
    /// **At most 64 fields.** The bookkeeping is one bit per field in a single
    /// `u64`. A struct with more fields than that read under this option is a
    /// compile error rather than a wider mask. The cap belongs to the option:
    /// no other setting looks at the field count, and a struct of any width
    /// still reads under every other policy. A `#[required]` field needs a bit
    /// only for itself, so a wider struct may still mark one, as long as what
    /// it marks is among the first 64 declared.
    const ERROR_ON_MISSING_KEYS: bool = false;

    /// Read JSONC: `//` to the end of the line, and `/* */`, anywhere
    /// whitespace is allowed.
    ///
    /// Off by default, a comment being no part of JSON. Turn it on with
    /// [`AllowComments`] for the documents people edit by hand, which is where
    /// the dialect comes from: a configuration file, a fixture, a schema kept
    /// under review.
    ///
    /// **Reading only.** Nothing writes a comment, because nothing holds one:
    /// a comment carries no data, so a document read under this and written
    /// back out comes back without it. **JSON only**, too. BEVE has no
    /// whitespace and so has nowhere to put one.
    ///
    /// A comment goes wherever whitespace goes: before a value, around a
    /// colon or a comma, between the last member and the closing brace. Not
    /// inside a string, where `//` is two ordinary characters and always was.
    ///
    /// A comment is stepped over only when it is *complete*. A `/` that begins
    /// nothing, and a `/*` that is never closed, are left exactly where they
    /// are for the reader to fail on, so the error carries the offset of the
    /// byte the comment began at rather than the end of the document.
    ///
    /// The streaming readers honour it too, and have to: they divide a stream
    /// into values before the parser sees any of it, and a comment may hold a
    /// brace. [`Mode::Lines`](crate::json::Mode::Lines) is the one place a
    /// comment cannot span lines, a value's bytes being a line there, and a
    /// comment between values is stepped over rather than checked as text,
    /// belonging to no value's span. See the guide for both.
    ///
    /// It costs one comparison per run of whitespace, against a byte already
    /// loaded, and nothing at all when it is off.
    const ALLOW_COMMENTS: bool = false;

    /// Count the bytes a document would occupy instead of assembling it.
    ///
    /// Not a setting to choose. It is how [`beve::size`](crate::beve::size)
    /// reuses the writer as a tape measure: the only policy that turns it on
    /// is private to this crate, and turning it on yourself would give you a
    /// writer that produces an empty document. It lives on this trait because
    /// the policy is the only type parameter every
    /// [`beve::Write`](crate::beve::Write) implementation already carries, so
    /// measuring reaches all of them without a signature changing anywhere.
    ///
    /// Honoured by the BEVE writer alone. The JSON side has no counterpart,
    /// framing there being done from a buffer.
    #[doc(hidden)]
    const MEASURE: bool = false;

    // Adding a constant here? If the BEVE writer will read it, forward it in
    // `Measured` below as well. A constant that reaches the writer but not the
    // forwarding makes `beve::size` describe a different document from the one
    // the same policy writes, which is a wrong length in a frame header and
    // nothing louder. Nothing enforces this: `SKIP_NULL` is the only constant
    // the BEVE writer currently reads, so a test cannot distinguish the rest.
}

/// `O`'s settings, with the writer counting bytes instead of storing them.
///
/// Private, and deliberately: it is the one policy that makes a writer
/// produce nothing, so the only things that may name it are the `size`
/// entry points in [`beve`](crate::beve).
///
/// **Every constant of [`Options`] must be forwarded**, or a measurement stops
/// describing the document the same policy would write.
///
/// Nothing checks that, and it is worth being exact about why rather than
/// claiming a test covers it. [`SKIP_NULL`](Options::SKIP_NULL) is the only
/// constant the BEVE writer reads at all, so every other policy in the crate
/// produces byte-identical binary and no test can tell a forwarded constant
/// from a dropped one: deleting the other six leaves the suite green. The list
/// is therefore a promise kept by hand, and the thing that would break it is a
/// *new* constant that the BEVE writer consults. The note at the foot of
/// [`Options`] is where that gets caught, because it sits where such a
/// constant would be added.
///
/// `fn() -> O` rather than `O`, matching the writer, so this type's auto
/// traits do not depend on a policy it never holds a value of.
#[derive(Clone, Copy)]
pub(crate) struct Measured<O: Options>(core::marker::PhantomData<fn() -> O>);

impl<O: Options> Options for Measured<O> {
    const PRETTY: bool = O::PRETTY;
    const INDENT: usize = O::INDENT;
    const NEW_LINES_IN_ARRAYS: bool = O::NEW_LINES_IN_ARRAYS;
    const SKIP_NULL: bool = O::SKIP_NULL;
    const ERROR_ON_UNKNOWN_KEYS: bool = O::ERROR_ON_UNKNOWN_KEYS;
    const ERROR_ON_MISSING_KEYS: bool = O::ERROR_ON_MISSING_KEYS;
    const ALLOW_COMMENTS: bool = O::ALLOW_COMMENTS;
    const MEASURE: bool = true;
}

/// Compact JSON, every declared member written, every unknown key refused.
///
/// The default everywhere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Standard;

impl Options for Standard {}

/// Indented JSON, two spaces per level.
///
/// ```
/// # #[derive(Default)]
/// # struct P { x: i32, y: Vec<i32> }
/// # structio::object!(P { x, y });
/// let out = structio::to_string_with::<structio::Pretty, _>(&P { x: 1, y: vec![2, 3] });
/// assert_eq!(out, "{\n  \"x\": 1,\n  \"y\": [\n    2,\n    3\n  ]\n}");
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Pretty;

impl Options for Pretty {
    const PRETTY: bool = true;
}

/// Indented JSON, with each array kept on one line.
///
/// [`Pretty`] in every other respect: two spaces per level, a space after
/// every colon, one object member per line. An array holds its elements on
/// the line its opening bracket sits on, separated by `, `, which is what
/// keeps a document of numeric data readable instead of a column one value
/// wide. An object inside an array still breaks across lines.
///
/// ```
/// # #[derive(Default)]
/// # struct Sample { id: u32, values: Vec<f64> }
/// # structio::object!(Sample { id, values });
/// let s = Sample { id: 7, values: vec![1.5, 2.5, 3.5] };
/// assert_eq!(
///     structio::to_string_with::<structio::PrettyInlineArrays, _>(&s),
///     "{\n  \"id\": 7,\n  \"values\": [1.5, 2.5, 3.5]\n}"
/// );
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PrettyInlineArrays;

impl Options for PrettyInlineArrays {
    const PRETTY: bool = true;
    const NEW_LINES_IN_ARRAYS: bool = false;
}

/// Compact, with null members left out.
///
/// ```
/// # #[derive(Default)]
/// # struct P { x: i32, note: Option<String> }
/// # structio::object!(P { x, note });
/// let p = P { x: 1, note: None };
/// assert_eq!(structio::to_string(&p), "{\"x\":1,\"note\":null}");
/// assert_eq!(structio::to_string_with::<structio::SkipNull, _>(&p), "{\"x\":1}");
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SkipNull;

impl Options for SkipNull {
    const SKIP_NULL: bool = true;
}

/// Unknown keys stepped over rather than refused.
///
/// The policy for reading a subset of a document you did not define, or one
/// written against a newer version of your schema.
///
/// ```
/// # #[derive(Default, Debug)]
/// # struct Port { port: u16 }
/// # structio::object!(Port { port });
/// use structio::{ErrorCode, SkipUnknown, from_str, from_str_with};
///
/// let doc = r#"{"port":8080,"debug":true}"#;
/// assert_eq!(from_str::<Port>(doc).unwrap_err().code, ErrorCode::UnknownKey);
/// assert_eq!(from_str_with::<SkipUnknown, Port>(doc).unwrap().port, 8080);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SkipUnknown;

impl Options for SkipUnknown {
    const ERROR_ON_UNKNOWN_KEYS: bool = false;
}

/// Every declared field required to be present, and every unknown key still
/// refused.
///
/// The policy for a document that is meant to carry the whole value rather
/// than a patch over one. It is the exact opposite of [`SkipUnknown`]: that
/// one accepts a document that says more than the schema does, this one
/// refuses a document that says less.
///
/// For a schema where only some members are mandatory, which is most of them,
/// mark those `#[required]` in the declaration and read under the default
/// policy. This is the blunt instrument, and it is the right one only where
/// every member really is mandatory.
///
/// ```
/// # #[derive(Default, Debug)]
/// # struct Point { x: f64, y: f64 }
/// # structio::object!(Point { x, y });
/// use structio::{ErrorCode, RequireKeys, from_str, from_str_with};
///
/// let doc = r#"{"x":1.0}"#;
/// assert_eq!(from_str::<Point>(doc).unwrap().y, 0.0);
/// assert_eq!(
///     from_str_with::<RequireKeys, Point>(doc).unwrap_err().code,
///     ErrorCode::MissingKey
/// );
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RequireKeys;

impl Options for RequireKeys {
    const ERROR_ON_MISSING_KEYS: bool = true;
}

/// JSONC: `//` and `/* */` comments accepted wherever whitespace is.
///
/// The policy for a document a person maintains rather than a program emits.
/// Everything else is unchanged, so an unknown key is still refused.
///
/// ```
/// # #[derive(Default, Debug)]
/// # struct Limits { retries: u8, timeout_ms: u32 }
/// # structio::object!(Limits { retries, timeout_ms });
/// use structio::{AllowComments, from_str_with};
///
/// let config = r#"{
///     // Give up after this many.
///     "retries": 3,
///     "timeout_ms": 500 /* per attempt */
/// }"#;
///
/// let limits = from_str_with::<AllowComments, Limits>(config).unwrap();
/// assert_eq!(limits.retries, 3);
/// assert_eq!(limits.timeout_ms, 500);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AllowComments;

impl Options for AllowComments {
    const ALLOW_COMMENTS: bool = true;
}
