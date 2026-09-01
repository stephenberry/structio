# Changelog

Notable changes to structio. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Before 1.0 the API is not frozen: a minor bump may break it, and what broke is listed here.

## [Unreleased]

### Fixed

- A declaration that names the same key or variant twice is a compile error even when the type is generic and never read. The check is in the key hash, which only reading used to reach.

### Added

- **Case rules.** `object!`, `unit_enum!`, `tagged_enum!` and their `json_`/`beve_` variants take one after the type, as in `object!(Root as "camelCase" { .. })`, and convert every key the declaration does not spell out. The eight `serde` spellings are accepted, an explicit `"key" => field` still wins, and the conversion happens during compilation, so the bytes match the same declaration with every key written out. The rule differs from serde's in three ways worth checking before porting a schema; see [docs/schemas.md](docs/schemas.md#case-rules).
- `Parser::read_number_str` and `Writer::write_number_str`: a number's text, borrowed and written verbatim, for a fixed-point, decimal, bignum, or rational type. JSON only.

## [0.1.0]

First release.

- **JSON and BEVE from one schema.** `object!`, `array!`, `unit_enum!` and `tagged_enum!` declare a type's fields once; both formats read and write against that declaration. Keys are hashed at compile time into a perfect hash chosen to fit the key set.
- **No dependencies and no proc-macros.** Standard library only. Rust 2024 edition, MSRV 1.96.
- **Reads reuse allocations.** `read_into` and `write_into` refill the buffers a value already holds, so a loop over records of one shape settles into no allocation.
- **Compile-time options.** Indentation, inline arrays, skipping null members, refusing unknown keys, requiring declared keys, and JSONC comments, as policy types resolved at compile time. Unused settings cost nothing.
- **BEVE beyond whole-document decoding.** `from_beve_at` reads the one value a JSON Pointer names, `validate_beve` checks a document without decoding it, `to_beve_aligned` writes numeric arrays a reader can borrow rather than copy, and `beve_slice_ref` takes that borrow.
- **Streaming in both formats, both directions.** `Documents` pulls values from a reader, `Feed` takes values out of chunks pushed at you, and `read_beve_array_into` reads a document that is one enormous numeric array without holding its encoded form.
- **`beve_to_json`** rewrites a BEVE document as JSON in one walk, with no schema and no tree.
- **Complex numbers and matrices.** `Complex<T>` and `Matrix<T>` cover BEVE's two data-carrying extensions, in both formats.
- **Errors carry a byte offset**, and `Error::display_with(input)` renders one with a line, column, and caret.

[Unreleased]: https://github.com/stephenberry/structio/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/stephenberry/structio/releases/tag/v0.1.0
