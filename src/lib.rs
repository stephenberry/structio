//! Structs in, structs out, in the spirit of
//! [Glaze](https://github.com/stephenberry/glaze).
//!
//! Values are read straight into your types and written straight out of them.
//! There is no `Value` enum, no token stream, and no document model: a field's
//! bytes are converted exactly once, into the member that will hold them.
//!
//! ```
//! #[derive(Default, PartialEq, Debug)]
//! struct Config {
//!     name: String,
//!     port: u16,
//!     hosts: Vec<String>,
//! }
//!
//! structio::object!(Config { name, port, hosts });
//!
//! let json = r#"{"name":"api","port":8080,"hosts":["a","b"]}"#;
//! let config: Config = structio::from_str(json).unwrap();
//! assert_eq!(config.port, 8080);
//! assert_eq!(structio::to_string(&config), json);
//! ```
//!
//! # Two formats, one schema
//!
//! [`object!`] declares a struct's field list once. Both formats read and
//! write against it, so the same type round-trips through either without a
//! second declaration:
//!
//! - [`json`] is JSON text.
//! - [`beve`] is [BEVE](https://github.com/stephenberry/beve), a tagged binary
//!   format that keeps JSON's shape and self-description while storing numbers
//!   as numbers and numeric arrays as contiguous blocks.
//!
//! ```
//! # #[derive(Default, PartialEq, Debug)]
//! # struct Sample { id: u32, values: Vec<f64> }
//! # structio::object!(Sample { id, values });
//! let sample = Sample { id: 7, values: vec![1.5, 2.5, 3.5] };
//!
//! let text = structio::to_string(&sample);
//! let binary = structio::to_beve(&sample);
//!
//! assert_eq!(structio::from_str::<Sample>(&text).unwrap(), sample);
//! assert_eq!(structio::from_beve::<Sample>(&binary).unwrap(), sample);
//! ```
//!
//! # Objects and arrays
//!
//! [`object!`] encodes a struct by key. [`array!`] encodes it by position,
//! for a type whose field names carry nothing a reader does not already have:
//!
//! ```
//! #[derive(Default, PartialEq, Debug)]
//! struct Vec3 { x: f64, y: f64, z: f64 }
//! structio::array!(Vec3 [x, y, z]);
//!
//! let v = Vec3 { x: 1.0, y: 2.0, z: 3.0 };
//! assert_eq!(structio::to_string(&v), "[1,2,3]");
//! ```
//!
//! Nothing is hashed and no keys go on the wire, at the cost of a schema that
//! cannot change shape: an array of the wrong length is an error, where an
//! object can be read with a policy that steps over a field it does not
//! recognize.
//!
//! # Enums
//!
//! An enum's schema is its variant names, and they go on the wire as names
//! rather than as positions. A variant carrying nothing is written as its
//! name; a variant carrying a value is written as an object of one member
//! keyed by that name, and is marked `(_)` in the declaration.
//!
//! ```
//! #[derive(Default, PartialEq, Debug)]
//! enum Shape {
//!     #[default]
//!     Empty,
//!     Sides(u32),
//! }
//! structio::tagged_enum!(Shape { Empty, Sides(_) });
//!
//! assert_eq!(structio::to_string(&Shape::Empty), "\"Empty\"");
//! assert_eq!(structio::to_string(&Shape::Sides(3)), r#"{"Sides":3}"#);
//! assert_eq!(structio::from_str::<Shape>(r#"{"Sides":3}"#).unwrap(), Shape::Sides(3));
//! ```
//!
//! [`unit_enum!`] is the same declaration for an enum whose variants all carry
//! nothing, and will not compile if one of them does, so its wire form is a
//! plain string and stays one.
//!
//! A variant carrying nothing reads back from either form, but one carrying a
//! value has only the object form, and the two macros' pages say what each
//! error means.
//! [docs/enums.md](https://github.com/stephenberry/structio/blob/main/docs/enums.md)
//! is the long form.
//!
//! The crate root re-exports the JSON entry points unqualified, because JSON
//! is what most callers want first. The BEVE ones carry the format in the
//! name at the root ([`to_beve`], [`from_beve`]) and drop it inside the
//! module, so [`beve::to_vec`] and [`to_beve`] are the same function.
//!
//! # Design
//!
//! - **No dependencies.** Standard library only.
//! - **No proc-macros.** [`object!`], [`array!`] and [`tagged_enum!`] are
//!   `macro_rules!` macros, so there is no proc-macro crate to compile and
//!   link before your code can build.
//! - **Keys are hashed at compile time.** [`KeyMap::build`] runs during const
//!   evaluation and picks the cheapest perfect hash that fits your key set,
//!   from a single byte comparison up to a full key hash. Both formats look
//!   keys up in that one table.
//! - **Reads reuse what you already own.** Parsing into an existing value
//!   refills its buffers instead of reallocating them.
//!
//! # What it does not do
//!
//! This is only for statically known types. There is no lazy or generic value
//! type: if you need to reach into arbitrary documents from code, this is the
//! wrong tool.
//!
//! A BEVE document can still be looked into without being decoded whole.
//! [`from_beve_at`] reads the one value a JSON Pointer names and steps over
//! everything else, and [`validate_beve`] checks a document is well formed
//! without decoding any of it. What comes back from the first is still a type
//! you declared.
//!
//! Reading a document you have no type for is a different matter, and
//! [`beve_to_json`] is the one entry point that hands back its *contents*
//! without one: it rewrites a whole BEVE document as JSON in a single walk. See
//! [`transcode`] for what survives the trip and what does not.
//!
//! # Complex numbers and matrices
//!
//! BEVE's core covers what JSON covers; its extensions cover what scientific
//! data needs on top of that. [`Complex`] and [`Matrix`] are those two, as
//! ordinary types that work in both formats:
//!
//! ```
//! use structio::{Complex, Matrix, MatrixLayout};
//!
//! let signal = vec![Complex::new(1.0f64, 2.0), Complex::new(3.0, -4.0)];
//! let bytes = structio::to_beve(&signal);
//! assert_eq!(structio::from_beve::<Vec<Complex<f64>>>(&bytes).unwrap(), signal);
//!
//! let m = Matrix::new(MatrixLayout::RowMajor, vec![2, 3], (0..6i32).collect()).unwrap();
//! assert_eq!(
//!     structio::to_string(&m),
//!     r#"{"layout":"layout_right","extents":[2,3],"value":[0,1,2,3,4,5]}"#
//! );
//! assert_eq!(structio::from_beve::<Matrix<i32>>(&structio::to_beve(&m)).unwrap(), m);
//! ```
//!
//! A run of complex numbers is one header and one block, so a
//! `Vec<Complex<f64>>` moves in a single copy exactly as a `Vec<f64>` does, and
//! a `Matrix<Complex<f64>>` stores its data that way with no case of its own.
//! See [`ext`].
//!
//! # Options
//!
//! Whether the JSON is indented, whether a member that would be null is
//! written at all, and whether a key nothing claims is refused, are decided at
//! compile time by a [policy type](Options). Every entry point has a `_with`
//! twin that takes one, and the plain one is [`Standard`]:
//!
//! ```
//! use structio::{Pretty, SkipNull, to_string, to_string_with};
//!
//! # #[derive(Default)]
//! # struct Server { port: u16, tls: Option<String> }
//! # structio::object!(Server { port, tls });
//! let server = Server { port: 8080, tls: None };
//!
//! assert_eq!(to_string(&server), r#"{"port":8080,"tls":null}"#);
//! assert_eq!(
//!     to_string_with::<Pretty, _>(&server),
//!     "{\n  \"port\": 8080,\n  \"tls\": null\n}"
//! );
//! assert_eq!(to_string_with::<SkipNull, _>(&server), r#"{"port":8080}"#);
//! ```
//!
//! Indentation puts every element of an array on a line of its own, which is
//! not what a run of numbers wants. [`PrettyInlineArrays`] keeps each array on
//! the line it began on and indents everything else:
//!
//! ```
//! use structio::{PrettyInlineArrays, to_string_with};
//!
//! # #[derive(Default)]
//! # struct Sample { id: u32, values: Vec<f64> }
//! # structio::object!(Sample { id, values });
//! let sample = Sample { id: 7, values: vec![1.5, 2.5, 3.5] };
//!
//! assert_eq!(
//!     to_string_with::<PrettyInlineArrays, _>(&sample),
//!     "{\n  \"id\": 7,\n  \"values\": [1.5, 2.5, 3.5]\n}"
//! );
//! ```
//!
//! Text that is already JSON takes the same policy from the other side.
//! [`prettify()`] lays out a document that did not come from a `Write` impl, and
//! emits its whitespace through the same writer, so the result is what writing
//! the same data would have produced:
//!
//! ```
//! use structio::{PrettyInlineArrays, json::prettify_with, prettify};
//!
//! assert_eq!(prettify(r#"{"a":[1,2]}"#).unwrap(), "{\n  \"a\": [\n    1,\n    2\n  ]\n}");
//! assert_eq!(
//!     prettify_with::<PrettyInlineArrays>(r#"{"a":[1,2]}"#).unwrap(),
//!     "{\n  \"a\": [1, 2]\n}"
//! );
//! ```
//!
//! [`minify()`] goes the other way, and has no layout to agree with: it copies
//! the document through and drops the whitespace between its tokens. That needs
//! nothing but the strings located, so it neither reads a value nor checks a
//! bracket, which is what makes it the fastest thing here.
//!
//! ```
//! assert_eq!(structio::minify("{\n  \"a\": [1, 2]\n}").unwrap(), r#"{"a":[1,2]}"#);
//! ```
//!
//! Reading takes a policy the same way. It has three settings, and only one
//! of them is on by default: a key that no field claims is an
//! [`ErrorCode::UnknownKey`], which catches a typo or the wrong document
//! rather than passing over it in silence. Ask for [`SkipUnknown`] to read a
//! subset of a larger document. The other way round, a declared field the
//! document leaves out is simply left as the destination had it, since reading
//! is into a value that already exists. Mark a field `#[required]` in the
//! declaration and its absence is an [`ErrorCode::MissingKey`] under every
//! policy, which is what a mixed schema wants; [`RequireKeys`] says the same of
//! every field at once.
//!
//! ```
//! use structio::{ErrorCode, SkipUnknown, from_str, from_str_with};
//!
//! # #[derive(Default, Debug)]
//! # struct Server { port: u16 }
//! # structio::object!(Server { port });
//! let doc = r#"{"port":8080,"debug":true}"#;
//!
//! assert_eq!(from_str::<Server>(doc).unwrap_err().code, ErrorCode::UnknownKey);
//! assert_eq!(from_str_with::<SkipUnknown, Server>(doc).unwrap().port, 8080);
//! ```
//!
//! ```
//! use structio::{ErrorCode, RequireKeys, from_str, from_str_with};
//!
//! # #[derive(Default, Debug)]
//! # struct Server { port: u16, host: String }
//! # structio::object!(Server { port, host });
//! let doc = r#"{"port":8080}"#;
//!
//! assert_eq!(from_str::<Server>(doc).unwrap().host, "");
//! assert_eq!(
//!     from_str_with::<RequireKeys, Server>(doc).unwrap_err().code,
//!     ErrorCode::MissingKey
//! );
//! ```
//!
//! The third is [`AllowComments`], which reads JSONC: `//` and `/* */`
//! wherever whitespace is allowed, for the documents people edit by hand.
//!
//! The setting you do not ask for costs nothing: a compact writer emits no
//! indentation code at all. Combinations are your own unit struct and an impl,
//! since every constant on [`Options`] has a default. See [`options`].
//!
//! # Streaming
//!
//! Documents that do not fit, or have not fully arrived, go through
//! [`json::stream`] and [`beve::stream`]: [`to_writer`] drains into an
//! [`std::io::Write`], [`Documents`] pulls a sequence of values out of an
//! [`std::io::Read`], and [`Feed`] takes chunks pushed at it. The BEVE
//! counterparts are [`beve::to_writer`], [`beve::Documents`] and
//! [`beve::Feed`], and they hand out the elements of a typed array one at a
//! time as readily as whole records. [`beve_to_json_writer`] drains a transcode
//! into a sink the same way.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod beve;
pub mod case;
pub mod error;
pub mod ext;
pub mod json;
pub mod keymap;
mod macros;
mod num;
pub mod options;
mod stream;
mod swar;
mod traits;
pub mod transcode;

pub use error::{Error, ErrorCode, Result, StreamError, StreamResult};
pub use ext::{Complex, Matrix, MatrixLayout, MatrixRef};
pub use keymap::KeyMap;
pub use options::{
    AllowComments, Options, Pretty, PrettyInlineArrays, RequireKeys, SkipNull, SkipUnknown,
    Standard,
};
pub use traits::{Elements, Keys, ReadWrite, Same, Variants};

pub use json::{
    Documents, Feed, Mode, from_reader, from_reader_with, from_slice, from_slice_with, from_str,
    from_str_with, minify, minify_into, minify_into_with, minify_with, prettify, prettify_into,
    prettify_into_with, prettify_with, read_into, read_into_with, to_string, to_string_with,
    to_vec, to_vec_with, to_writer, to_writer_buffered, to_writer_buffered_with, to_writer_with,
    write_into, write_into_with,
};

// The BEVE entry points carry their format in the name at the root, where
// they sit beside the unqualified JSON ones, and drop it inside the module.
pub use beve::append as append_beve;
pub use beve::append_aligned as append_beve_aligned;
pub use beve::append_aligned_with as append_beve_aligned_with;
pub use beve::append_with as append_beve_with;
pub use beve::from_reader as from_beve_reader;
pub use beve::from_reader_array as from_beve_reader_array;
pub use beve::from_reader_with as from_beve_reader_with;
pub use beve::from_slice as from_beve;
pub use beve::from_slice_at as from_beve_at;
pub use beve::from_slice_at_with as from_beve_at_with;
pub use beve::from_slice_with as from_beve_with;
pub use beve::read_array_into as read_beve_array_into;
pub use beve::read_into as read_beve_into;
pub use beve::read_into_at as read_beve_into_at;
pub use beve::read_into_at_with as read_beve_into_at_with;
pub use beve::read_into_with as read_beve_into_with;
pub use beve::size as beve_size;
pub use beve::size_after as beve_size_after;
pub use beve::size_after_with as beve_size_after_with;
pub use beve::size_aligned as beve_size_aligned;
pub use beve::size_aligned_after as beve_size_aligned_after;
pub use beve::size_aligned_after_with as beve_size_aligned_after_with;
pub use beve::size_aligned_with as beve_size_aligned_with;
pub use beve::size_with as beve_size_with;
pub use beve::slice_ref as beve_slice_ref;
pub use beve::to_vec as to_beve;
pub use beve::to_vec_aligned as to_beve_aligned;
pub use beve::to_vec_aligned_with as to_beve_aligned_with;
pub use beve::to_vec_with as to_beve_with;
pub use beve::to_writer as to_beve_writer;
pub use beve::to_writer_buffered as to_beve_writer_buffered;
pub use beve::to_writer_buffered_with as to_beve_writer_buffered_with;
pub use beve::to_writer_with as to_beve_writer_with;
pub use beve::validate as validate_beve;
pub use beve::validate_reader as validate_beve_reader;
pub use beve::write_into as write_beve_into;
pub use beve::write_into_with as write_beve_into_with;

// Neither format's module owns this one, since it names both.
pub use transcode::{
    beve_to_json, beve_to_json_into, beve_to_json_into_with, beve_to_json_with,
    beve_to_json_writer, beve_to_json_writer_buffered, beve_to_json_writer_buffered_with,
    beve_to_json_writer_with,
};
