# Changelog

Notable changes to structio. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Before 1.0 the API is not frozen: a minor bump may break it, and what broke is listed here.

## [Unreleased]

### Changed

- **A raw identifier's `r#` is no longer part of its key.** A field or variant written `r#type` had the key `r#type`, because that is what `stringify!` hands the macro. It is now `type`, before any case rule runs, since the prefix is how Rust spells a name that collides with a keyword rather than part of the name: `r#type` is how you write a field for a `"type"` key. Both formats, fields and variants, derived and declared. An explicit `"r#type" => field` is a literal and is unchanged. **Breaking** for a declaration with a raw identifier and no explicit key, which now reads and writes a different key.

- **An internally tagged enum's tag no longer has to come first.** `tagged_enum!(.. as tag "kind")` used to refuse an object whose first member was not the tag with `ExpectedTag`, which refused every document from a sorted-key writer the moment a member sorted before the tag. The reader now steps over the members before the tag, dispatches on it, reads the members after it, and then reads the ones it stepped over, nesting as deep as the payloads do. A tag that is first still costs one pass; the members before a late tag are walked twice, and a key on both sides of the tag keeps its earlier value. Required-field and unknown-key rules apply to the deferred members as to any other. An object with no tag at all is still `ExpectedTag`, reported against its first key.

### Added

- **`#[derive(Structio)]`, behind the `derive` feature.** A front end to `object!`, `array!`, `unit_enum!` and `tagged_enum!`: it reads the type and emits the declaration, so a derived type and a declared type are the same impls. `rename_all`, `tag`, `array`, `element`, `json`, `beve` and `crate` on the type; `rename`, `skip`, `required` and `with` on a field; `rename` on a variant. Generics and their bounds are read off the type. The feature is off by default and the derive crate has no dependencies. [docs/derive.md](docs/derive.md) has the rest, including what later stages add.
- **BEVE containers reserve on the wire count.** `Reader::read_seq_counted` and `read_map_counted` hand the element count to the caller before the first element, clipped to what the input could hold, and `beve::cautious::<T>` clips it again to a megabyte of `T`. `Vec`, `VecDeque`, `HashMap` and `HashSet`, adapted or not, reserve once instead of doubling up; a hostile count can waste at most that megabyte.
- **`Value`, a tree for a value with no declared type.** Null, bool, number, string, array, object, with `get`, `pointer`/`pointer_mut`, the `as_*`/`is_*` accessors, `Index`/`IndexMut` by key or position, and the `value!` macro to build one. It reads and writes through both formats like any other type, so it can be a field of an `object!` declaration or a whole document; a BEVE typed array, complex run or matrix reads into the same shape `beve_to_json` writes. `Number` keeps whether it was an unsigned integer, a negative integer or a float, and writes a whole-valued float as `1.0` so the kind survives a trip through text. `to_value` and `from_value` move a declared type in and out, through JSON text. This is for the value nothing decodes, a register tree walked by path or a body forwarded unread, not a substitute for a declared type, and the crate's stance on that is unchanged.

## [0.3.2] - 2026-09-03

### Changed

- **Faster JSON reading and string writing.** Against Glaze on the benchmark documents, reading doubles went from 58% to 87% of its speed, mixed documents from 78% to over 100%, and signed integers, bools and strings moved up with them; string writing went from 91% to 94%. The float reader is now inlined into the array loop rather than called per element, out-of-line helpers no longer pin the parser's cursor to the stack, signs and bools are read without a branch on the data, digits are folded a word at a time rather than one at a time, integers of up to fifteen digits stay on the inlined path, and strings are copied as they are scanned. [docs/performance.md](docs/performance.md) has the measurements and the mechanisms. Output and accepted input are unchanged, and the float scanner is checked bit for bit against the standard library on 200,000 generated literals.

## [0.3.1] - 2026-09-03

### Changed

- **A borrowing type names its lifetime as it likes.** `object!(['a] Borrowed<'a> { .. })` now works, as do `array!`, `tagged_enum!` and their single-format forms: the first lifetime in the bracket is the input lifetime, whatever it is called. It had to be spelled `'de`, and any other name failed from inside the expansion with "lifetime may not live long enough" and no hint about why. Declarations written with `'de` are unchanged.

### Added

- `json::MAX_DEPTH` and `beve::MAX_DEPTH`, the nesting limit each reader enforces, re-exported at the module root. They were reachable only through `json::parser` and `beve::reader`.

## [0.3.0] - 2026-09-02

### Added

- **Internal tagging**, a second convention for `tagged_enum!`, asked for with a tag clause: `tagged_enum!(Shape as tag "kind" { .. })`. The variant name goes inside the payload's object as a member rather than wrapping it, giving `{"kind":"Circle","radius":1}` where the clause-free form writes `{"Circle":{"radius":1}}`. This is what most JSON APIs use, and the only form here that a C++ Glaze `std::variant` can be made to agree with, external tagging having nowhere to put the payload's own keys. The clause works on `json_tagged_enum!` and `beve_tagged_enum!` too.

  **The tag has to be the object's first member**, and a document that puts it elsewhere is the new `ErrorCode::ExpectedTag`, reported against the offending key. Reading is one pass with no lookahead, so a tag arriving after the members it gives meaning to could only be used by holding the object or walking it twice. Writing always emits the tag first, so this crate's own output round-trips unconditionally, as does any producer that emits its tag first — the conventional ordering. The refusal is loud and positioned rather than a misparse.

  A payload must be an object (a compile error naming `WriteObject` otherwise), since its members share the object with the tag. Everything else carries over: renaming, case rules, generics, borrowed payloads, reading into an existing value, and the policies. The result is an ordinary object, so pointers, validation and transcoding walk it with no knowledge of enums at all.

- `Parser::read_object_rest` and `Parser::finish_internally_tagged`, their `Reader` counterparts, and `Writer::write_internally_tagged` in both formats, for hand-written impls of the two new `ReadInternallyTagged` traits. A variant carrying nothing writes through the existing `write_tagged`, the bytes being the same object of one member.

- A tag that is also a field of a variant's payload is a **compile error**. The two share one object, so it would write the name twice; structio reads that back and a last-wins parser does not, keeping the field and losing the variant. The comparison is of wire names, so a collision that only appears after a case rule is caught too. `cargo check` refuses a declaration with no generics; a generic one is refused when the crate is built, a generic payload having no keys until it is instantiated.

## [0.2.2] - 2026-09-02

### Changed

- Reading arrays of integers is 38-43% faster, and the representative `mixed` document 11%. The element loop was making a function call per element, which spilled the parser's cursor to the stack and reloaded it on every return; `parse_u64` now keeps a small fast path for the common short number and hands the rare cases to an out-of-line one, which is enough for the whole read to inline. JSON whitespace is answered from a table rather than a bitmask that needed a range guard in front of it. No API or output change.

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

[Unreleased]: https://github.com/stephenberry/structio/compare/v0.3.2...HEAD
[0.3.2]: https://github.com/stephenberry/structio/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/stephenberry/structio/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/stephenberry/structio/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/stephenberry/structio/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/stephenberry/structio/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/stephenberry/structio/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/stephenberry/structio/releases/tag/v0.1.0
