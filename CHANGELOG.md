# Changelog

Notable changes to structio. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Before 1.0 the API is not frozen: a minor bump may break it, and what broke is listed here.

## [Unreleased]

### Fixed

- `read_array_into` and `from_reader_array` read a **complex array**. They read the typed-array tag and stopped at the extension's, so the one shape that most needs a streaming block read — a buffer of IQ samples, which a consumer can least afford to hold twice — had to go through `from_reader` and hold the encoded document alongside the vector. The payload was always a block: interleaved `(re, im)` components are the in-memory form of `[Complex<T>]` for the same reason a typed array's payload is the in-memory form of `[T]`. Only the preamble differed.

  The aligned complex form is byte-identical to the plain one, so both arrive by the same path. `COMPLEX_ONE` is refused as `InvalidHeader`, being a lone value with no count rather than an array, as are the six undefined forms of the class byte.

- The big-endian conversion in the same read reverses each **component** rather than each element. It could not have fired before, no complex array having reached it, but a `Complex<f32>` is eight bytes and reversing all eight would have transposed `re` and `im` as well as swapping the bytes of each. For every other numeric type the component is the element, so one stride serves both.

## [0.2.1] - 2026-09-02

### Changed

- `Documents::read_size` sizes the window as well as the read. The buffer is allocated on the first fill and holds one chunk, so `Documents::array(bytes).read_size(4096)` costs 4 KiB rather than the 64 KiB it allocated up front before, whatever the read size said. This is the knob for decoding a small document that is already in memory, where a default window is a thousand times the document. Applies to both `json::Documents` and `beve::Documents`; `Feed` is unchanged, having no chunk size to go by.
- `beve::Reader::read_seq` documents that element positions do not bound documents. A typed array's element headers, a complex array's, and a boolean run's are supplied by the reader rather than present in the input, so a span cut between two `position()` calls is not a value `Reader::new` can read. Use `Documents::array` to take elements as documents of their own.

## [0.2.0] - 2026-09-02

### Added

- `json::append(&T, &mut Vec<u8>)` writes a document after what a buffer already holds, the counterpart of `beve::append`. `write_into` replaces a buffer's contents, so a value that has to sit behind a protocol header or behind the entries already in a listing needed a second buffer and a copy out of it. `json::Writer::appending` is the same thing with the writer in hand.

### Changed

- `json::append`, `beve::append` and `beve::append_aligned` leave the buffer exactly as they found it if writing the value panics. The buffer moves into the writer, so an unwind used to drop it along with the bytes in front of the document -- a header, or the entries already in a listing -- which the call was never meant to touch. A `Write` impl may panic by design: an adapter whose target has values it cannot encode is told to substitute or panic. `write_into` still leaves its buffer empty there, its contents being the call's to replace, and now says so.
- `json::Writer::into_string` checks the bytes handed to `Writer::appending`, and panics if they are not UTF-8. Every other byte in the buffer is UTF-8 by construction; those are the only ones the writer did not produce. Use `into_vec` to append JSON behind a binary prefix.

## [0.1.0] - 2026-09-01

First release.

- **JSON and BEVE from one schema.** `object!`, `array!`, `unit_enum!` and `tagged_enum!` declare a type's fields once; both formats read and write against that declaration. Keys are hashed at compile time into a perfect hash chosen to fit the key set.
- **No dependencies and no proc-macros.** Standard library only. Rust 2024 edition, MSRV 1.96.
- **Declarations are checked against the type.** Leaving out a field, or naming the same key twice, is a compile error that names what is wrong. End a declaration with `..` where the omission is deliberate: `object!(Config { host, port, .. })`.
- **Case rules.** `object!(Root as "camelCase" { .. })` converts every key the declaration does not spell out, in the eight `serde` spellings, during compilation. See [docs/schemas.md](docs/schemas.md#case-rules) for three ways the rule differs from serde's.
- **Reads reuse allocations.** `read_into` and `write_into` refill the buffers a value already holds, so a loop over records of one shape settles into no allocation.
- **Compile-time options.** Indentation, inline arrays, skipping null members, refusing unknown keys, requiring declared keys, and JSONC comments, as policy types resolved at compile time. Unused settings cost nothing.
- **BEVE beyond whole-document decoding.** `from_beve_at` reads the one value a JSON Pointer names, `validate_beve` checks a document without decoding it, `to_beve_aligned` writes numeric arrays a reader can borrow rather than copy, and `beve_slice_ref` takes that borrow.
- **Streaming in both formats, both directions.** `Documents` pulls values from a reader, `Feed` takes values out of chunks pushed at you, and `read_beve_array_into` reads a document that is one enormous numeric array without holding its encoded form.
- **`beve_to_json`** rewrites a BEVE document as JSON in one walk, with no schema and no tree.
- **Complex numbers and matrices.** `Complex<T>` and `Matrix<T>` cover BEVE's two data-carrying extensions, in both formats.
- **Errors locate themselves.** `Error` carries a byte offset, `Error::display_with(input)` renders one with a line, column, and caret, and a `MissingKey` names the absent key.
- `Parser::read_number_str` and `Writer::write_number_str`: a number's text, borrowed and written verbatim, for a fixed-point, decimal, bignum, or rational type. JSON only.

[Unreleased]: https://github.com/stephenberry/structio/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/stephenberry/structio/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/stephenberry/structio/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/stephenberry/structio/releases/tag/v0.1.0
