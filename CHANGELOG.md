# Changelog

Notable changes to structio. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Before 1.0 the API is not frozen: a minor bump may break it, and what broke is listed here.

## [Unreleased]

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

[Unreleased]: https://github.com/stephenberry/structio/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/stephenberry/structio/releases/tag/v0.1.0
