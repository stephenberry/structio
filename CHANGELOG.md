# Changelog

Notable changes to structio. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Before 1.0 the API is not frozen: a minor bump may break it, and what broke is listed here.

## [Unreleased]

### Changed

- **A pointer read is charged the containers it passes through.** `from_beve_at` and `Reader::seek` counted no level for the path, and stepped over siblings as if from the top, so the depth limit applied from the value named rather than to the document. `from_beve_at` now counts every container on the way, and a document every other walk refuses as too deep is refused here too. A hand-driven `seek` measures siblings on the path the same way but leaves the reader's depth as it found it. **Breaking** for a document past the limit that was readable through a pointer.

- **`json::prettify` and `json::minify` are only the functions.** Each name was also a public module holding nothing but the functions already re-exported beside it, so rustdoc listed it twice and a doc link to it was ambiguous. The modules are private now, and the docs they carried are on the functions. **Breaking** for a path through the module, such as `json::prettify::prettify` or `structio::minify::minify_with`: drop the module.

- **A BEVE header with an unspecified bit set is `InvalidHeader`.** The specification requires those bits to be zero, but every walk, `validate_beve` included, read a string or generic array with any of its top five bits set, or a string-keyed object with any of its top three, as the plain header, so one value had many encodings. They are now refused on the header, as an undefined width is. An object of the undefined fourth key type is `InvalidHeader` too, rather than `UnsupportedKeyType`. **Breaking** for documents that set those bits, which this crate never writes.

- **A packed-boolean array with non-zero padding is `InvalidPadding`.** The specification requires the bits past the last element in the final byte to be zero, but every walk ignored them, so one array had up to 128 encodings. Every walk now refuses a set one, just past the array's last byte, or at the value's first byte from a stream's framer. A pointer into a packed-boolean array now needs the whole array present. **Breaking** for documents with non-zero padding, which this crate never writes.

- **An aligned typed array padded by its element's width or more is `InvalidPadding`.** The specification bounds the padding length below the element's alignment, but every walk took up to 255. Every walk now refuses it just past the length byte, or at the value's first byte from a stream's framer. Only the range is checked: the padding's contents are ignored, and the exact length an encoder picks depends on an offset a reader cannot always know. **Breaking** for documents padded that far, which no conforming encoder writes.

### Added

- **`ErrorCode::InvalidPadding`,** for the two refusals above.

### Fixed

- **An unsigned integer reads `-0` as `0`,** as a signed one does, at every width, as a value and as an integer key: a JSON key, a BEVE string key, or a BEVE pointer token naming a key. It is still no array index, which RFC 6901 spells without a sign. It was `NumberOutOfRange`, or `ExpectedNumber` for a `u128`. A negative number is `NumberOutOfRange` at every width, a `u128` included, and a sign in front of a malformed number, such as `-` or `--1`, is refused the same way at every width, signed or not.

- **A fraction or an exponent makes an integer `InvalidNumber` at every width, however large.** `-1.5` into an unsigned type, and a number too large for the parse, such as `18446744073709551616.5` into a `u64`, were `NumberOutOfRange`. `NumberOutOfRange` is now reported where the digits end, as it was for a value past a narrow type's range: `-1` into a `u8` at offset 2 rather than 0, and a number past a `u64` at its end rather than its start.

- **The `Value` docs say what reading `-0` as `-0.0` costs.** Behaviour is 0.7.0's, unchanged: `as_i64`, `as_u64` and `is_i64` refuse it, `from_value` into an integer type refuses it while `from_str::<i64>("-0")` is `0`, and it writes to BEVE as an `f64` rather than the integer `0`. An integer past 64 bits, stored as a float, has always behaved the same way.

- **An undefined BEVE header is `InvalidHeader` in every walk, just past the header,** or at the value's first byte from a stream's framer, which reports every refusal there. A typed read that wanted that kind of value reported a mismatch instead: `ExpectedInteger` for a float of undefined width (a 0.7.0 regression), `ExpectedBool` for an undefined null or boolean, `UnsupportedKeyType` for a struct or enum given integer keys of undefined width, and `ExpectedBytes` for a `&[u8]` given an undefined typed array. A typed array of an undefined element type is now refused on its header, before its count.

- **A 128-bit float is refused on its header by every walk that decodes it,** rather than past its payload: `Value` and `beve_to_json` as the typed readers refuse it, and a typed read of a typed or complex array of them before it looks for the payload. A truncated one is `UnsupportedFeature` rather than `UnexpectedEnd`.

- **At the depth limit, a typed array is refused for its depth in every walk.** The stream framer checked the element type before charging the level, so an undefined one was `InvalidHeader` there and `ExceededMaxDepth` everywhere else.

- **`Documents::array` and `Feed::array` charge the outer array a level.** Its elements were framed as if at the top, so a document one level past the limit streamed, and at the limit the two modes refused the same bytes differently.

- **A BEVE element's header no longer outlives the element.** An element of a typed array refused without taking its header, as `Matrix` refuses one, left that header installed, and so did `Reader::seek` onto an element with nothing failing, so after a `rewind` the next read took it as its own: a retry failed differently, a second seek found no array, and a number could read from the wrong bytes without an error. `rewind` now puts an element's header back when it lands on the element, so a reader can try one type and then another on it, and takes it away anywhere else.

- **`Matrix`, `Complex` and `()` report a BEVE value of the wrong kind just past its header,** as every other reader does, rather than on the header. `()` also refuses an undefined null or boolean header as `InvalidHeader`, as every other walk does.

## [0.7.0] - 2026-09-24

### Changed

- **A BEVE delimiter is never a value.** `validate_beve` accepted a document that was only a delimiter, which no reader could read and no stream framed. Everywhere a value belongs, every walk now refuses one as `InvalidHeader`; between streamed documents it is still a separator. **Breaking** for code expecting `UnsupportedFeature` for a delimiter from a `Value` read or `beve_to_json`, or relying on a delimiter as an unknown member's value being stepped over.

### Fixed

- **A failed read no longer costs a reader that winds back and retries.** Every reader that entered a nesting level kept it on an error path, so a speculating reader lost a level per failed attempt and, after 256, had ordinary input refused as `ExceededMaxDepth`. A failed internally tagged read with a late tag could also leave its held members behind for the next object at the same depth, which then read them as its own.

- **`json::Raw` checks string escapes.** `Raw::new`, `Raw::from_string` and a `Raw` field accepted `"\q"` or a lone `"\ud800"` and wrote it back out. They now refuse any escape the string reader refuses, with the same error. The number grammar is still not checked.

- **A typed array read in one copy, borrowed, or read as a `&[u8]` is charged a nesting level, as every other walk charges it.** A document one level from the limit read with `from_beve` but failed `validate_beve`, `beve_to_json` and framing, and a framing failure ends a `Documents` or `Feed` stream.

- **`-0` read into a `Value` lost its sign.** It became the integer `0`; it is now the float `-0.0`, as `-0.0` and `-0e0` already were and as an `f64` reads it.

- **A BEVE float read as an integer, or a 128-bit float read at all, no longer reports an offset past the value.** Both are refused on the header before the payload is taken, so the cursor and `Error::index` stop just past the header, as every other type mismatch does.

## [0.6.0] - 2026-09-18

### Added

- **A field or a variant may answer to more than one name.** `object!(Settings { timeout | "timeout_ms" })` and `tagged_enum!(Mode { Idle | "idle" })`, repeatable, with `#[structio(alias = "..")]` as the derive's spelling. The declared name is the one written and an alias is only ever read, so adding one cannot change a byte the program writes: it is how a key or a variant is renamed without breaking the documents already written under the old spelling. A case rule leaves an alias alone, as it leaves an explicit key alone; a `#[required]` member is satisfied by any of its names; a `write_only` declaration refuses one, never reading. `Keys::ALIASES` and `Variants::ALIASES` are the new associated consts that carry it, both defaulted to empty, so a hand-written impl is unchanged. [docs/schemas.md](docs/schemas.md#more-than-one-key-for-a-field) has the rest.

- **`transparent!`, a one-field struct written as that field alone.** `UserId(7)` is `7`, not `[7]` and not `{"0":7}`: reading and writing delegate to the field, with no object, no keys and no array around it. That is the declaration for the newtype that exists to keep two `u64`s apart in Rust and means nothing to either format, which [`array!`](docs/schemas.md#positional-structs) writes as a one-element array and an object cannot write at all, having no name for its key to be. `json_transparent!` and `beve_transparent!` narrow it to one format and `write_only` to one direction; it takes an adapter, `transparent!(Timeout { 0 as Millis })`, which is the whole newtype-around-a-foreign-type case in one line; `#[structio(transparent)]` is the derive's spelling. `is_null` is forwarded in both formats, and BEVE's typed-array path deliberately is not, that one being a layout claim a declaration cannot make. [docs/schemas.md](docs/schemas.md#a-wrapper-that-is-not-on-the-wire) has the rest.

- **`ReadOwned`, the bound for parsing a value out of a buffer you own.** At the crate root and per format, as `ReadWrite`'s read-side counterpart. A function that holds the document and hands a `T` back needs `for<'de> Read<'de> + Default`: higher-ranked because a type may borrow out of the input and a caller owning the buffer cannot allow that, and `Default` because reading fills a value rather than constructing one. The crate spelled that pair out in ten of its own signatures, and a downstream extractor had to work it out from scratch; `ReadOwned` is the name for it. `from_str` still takes `Read<'de>`, tied to the input's lifetime so a borrowing type can be read, exactly as serde keeps `DeserializeOwned` to `from_reader`. [docs/schemas.md](docs/schemas.md#default-is-required-where-values-are-constructed) has the rest.

- **`json::Raw::from_string` and `from_string_unchecked`, the way in from a `String`.** Text this program produced rather than read had no entry point: `Raw::new_unchecked(&text).into_owned()` copied a buffer the caller already owned, there being no way to hand the `String` over. These take it, so the buffer becomes the span with nothing reallocated and the span never copied out of it, `from_string` trimming by shifting bytes inside it. The check and the trimming are `new`'s, being the same walk. The `Raw` then holds the caller's whole allocation rather than just the span, so `new(&s)?.into_owned()` is still the call for a long-lived value whittled out of a much larger buffer. A rejected value is dropped rather than handed back, so check with `new` first where the text has to survive its own rejection. There is still no `From<String>`, for the reason there is no `From<&str>`.

- **`Display` for `json::Raw` and `json::JsonStr`.** `Raw` displays its span, which is what `as_str` gives and what a compact write emits, escapes and quotes included, since the type is about the spelling. `{:#}` is that same text rather than a laid-out one: a span `new_unchecked` accepted may have no layout, and `Display` has nowhere to report that, so `prettify` stays the named way to ask. `JsonStr` displays the string the document meant, with its escapes already resolved.

- **`Debug`, `Clone`, `PartialEq`, `Eq` and `Hash` for `json::JsonStr`.** It had none, so a test could not `assert_eq!` on one or print one, and the key `Error::key_in` hands back could not be looked up in a set of the names a schema knows. Equality and hashing are the text rather than the variant: a key written `"a"` and one written `"\u0061"` are the same key, which deriving either would have denied.

### Fixed

- **`ReadWrite`'s doc example was missing `+ Default` and could not compile.** It was marked `ignore`, so nothing caught it. Now compiled, and the same example in the `object!` docs already had the bound. `Write`'s example, the crate's only other `ignore`, is compiled too.

## [0.5.0] - 2026-09-18

### Changed

- **`Value::Object` keeps its member order.** It is an `OrderedMap<Value>` rather than a `BTreeMap<String, Value>`, so a document read into a `Value` and written back out lists its members in the order it arrived in, and `to_value` yields a declared type's field order. Equality is unchanged in meaning and ignores order, so two values can compare equal and write different text. `sort_keys()` on the object gives back the sorted output. **Breaking** for code naming `BTreeMap` where `structio::Object` is expected, or relying on sorted output.

- **`transparent` is stage 2 of the derive, not stage 3.** It describes the whole type rather than one field: stage 2 is for a shape the macros cannot declare, stage 3 for per-field policy. It is not implemented and the derive still refuses it, now naming stage 2.

### Added

- **A `Value` compares with the primitive it holds.** `doc["port"] == 8080` and `8080 == doc.get("port").unwrap()` are comparisons now rather than a `value!(8080)` to wrap the right-hand side in: every integer width, `f32`, `f64`, `bool`, `str`, `&str`, `String` and `&String`, on either side, with the value owned or behind either reference the accessors hand back. Only `str`, `&str` and `bool` had an impl before, and only against an owned value, so most of the table was a compile error and which part was a matter of luck. A number is met at the comparand's width: `value!(1) == 1.0` and `value!(1.0) != 1`, being the different numbers this crate keeps them, and against an `f32` the stored number rounds to that width, so a document's `0.1` equals `0.1f32` while one too large or too small to round to an `f32` equals none. **Breaking** for a caller whose right-hand type was pinned by there being one `PartialEq` impl within reach: with `n: u64`, `n == w.into()` is now ambiguous between `u64` and `Value` and needs the type named.

- **`Error::key_in`, the name an unknown key had.** `UnknownKey` and `UnknownVariant` carry no name, a name the document chose being no `&'static str`, so the error winds back to it and reports the offset. This reads it back out of the document at that offset, unescaping as the reader would, and hands a `MissingKey` the static name it already carries. The lifetime is the document argument's, not the error's, so `Error` stays `Copy` and independent of the buffer. JSON only: a BEVE key's length lives in a prefix the offset is already past.

- **`read_map_located`, on `json::Parser` and `beve::Reader`.** `read_map` calls back after the colon, so a hand-written map reader could see a key's name but not its position, and hand-rolling the loop was no way out either, the depth counter that bounds nesting not being public. This reports the offset alongside the key: the same byte a generated reader's `UnknownKey` names, so an error raised by hand reads like one the crate raised.

- **`structio::OrderedMap<V>`, a string-keyed map that keeps its insertion order.** Entries live in one vector in the order they arrived; below nine keys a lookup scans it, above that a robin hood hash table indexes it. It reads and writes in both formats, so it works as a whole document or as a declared field where `BTreeMap` would, and `Object` is `OrderedMap<Value>`. Equality ignores order; `sort_keys()` reorders by key.

- **`write_only`, a declaration of the write half alone.** `object!(write_only ..)`, and the same token in front of `array!`, `unit_enum!`, `tagged_enum!` and the one-format macros, generate the writing impls and no read at all, so a field's type needs no `Read` impl and no `Default`. `#[structio(write_only)]` is the derive's spelling of it, and the bytes written are unchanged either way. A generic one bounds its type parameters by the new `structio::Write` rather than by `structio::ReadWrite` and `Default`. `#[required]` is refused, being a rule about reading, and there is no `read_only`. [docs/schemas.md](docs/schemas.md#one-direction-only) has the rest.
- **A failed `Read` or `Write` bound now explains the direction axis.** `Read`, `Write`, `ReadAs` and `WriteAs`, in both formats, carry `#[diagnostic::on_unimplemented]` notes naming what discharges the bound: `write_only` where the struct is only ever written, a `skip_value()` stub where one field is, and on the write side `write_null()` with `is_null` returning `true`, since a member that writes nothing truncates the object.
- **`json::Raw`, one JSON value carried through as its text.** A field that captures the exact bytes of a value on read and emits them unchanged on write, so a forwarded body keeps its key order, its number spellings and its escapes: what `Value` is not, being a tree that respells its numbers and decodes its escapes. Reading borrows the span out of the document, and under `ALLOW_COMMENTS` strips the comments out of a span that carries any, owning that one; writing is one copy of those bytes, laid out again at the right depth under `PRETTY`. JSON only, so a struct with one is declared with `json_object!`. [docs/schemas.md](docs/schemas.md#json-that-goes-through-untouched) has the rest.
- **`json::prettify_value_into`.** Lays one JSON value out into a `Writer` that is already part-way through a document, at that writer's current depth and under its policy. `json::Raw` writes through it under `PRETTY`, and it is what a passthrough type of your own needs so that a forwarded value is indented against its neighbours rather than emitted as a blob.
- **Tuple structs, in `array!` and in the derive.** A positional declaration names a field by its position, `array!(Entry [0, 1])`, so the shape that has no field names is now the shape it takes most naturally: `#[derive(Structio)]` with `#[structio(array)]` accepts a tuple struct, with the order, `..`, an element type and generics all as they are for a named struct. The bytes are the tuple's, as they always were. Declared as an object it is still refused, an object having nothing for its keys to be, and the message now names `#[structio(array)]`.
- **`Writer::member_key`, for an object key known only at run time.** `member` takes the key already prepared, quoted with its colon in JSON and length-prefixed in BEVE, because that is what a declaration assembles at compile time. A hand-written `WriteObject` whose keys come off a walk had to build those bytes itself, and it went wrong differently in each format: nothing escapes a JSON key on that path, so a key holding a `"` or a `\` wrote a document no reader takes, and a BEVE member laid down with `size` and `raw` is not counted, which a debug build catches against the header the object already committed to. This takes the key itself, escapes or length-prefixes it, counts the member, and honours `SkipNull` exactly as `member` does; `member_key_with` is the adapter form. The policy's boundary is a struct's member against a map's entry, not a compile-time key against a computed one, and `write_keyed` is still the map. [docs/schemas.md](docs/schemas.md#a-key-known-only-at-run-time) has the rest.
- **`json::Parser::rest_str`.** The `&str` counterpart of `rest`, for a hand-written `Read` impl capturing a span: the input's UTF-8 validity is already known, so nothing has to establish it a second time.

### Fixed

- **A `Matrix` names the unknown key it refused.** It reads its three members by hand through a map callback, which runs after the colon, so its `UnknownKey` reported the offending member's *value* rather than its key: a caret under the value, and `key_in` reading a name off it. It winds back to the key, so every `UnknownKey` in the crate now names a key. The reported offset for this one code on this one type moves.

- **The whole-key hash now folds in the key length.** It read whole 8-byte chunks and then an overlapping tail, so two keys differing only in the bytes between them, such as `field_name_10500_value` and `field_name_105000_value`, hashed the same under every seed and cost the object its hash: it read under a linear scan instead. Keys shorter than 8 bytes were zero filled, so trailing NULs vanished the same way. Never a wrong field, since a candidate is always confirmed by a full comparison, only a slower read.

- **A declared type does not need `Default`.** [docs/derive.md](docs/derive.md) said it did, flatly, contradicting [docs/schemas.md](docs/schemas.md). `Default` is required where a read constructs a value: the entry points that return one, an `Option`'s payload, a growing `Vec`'s tail, a map's values, an enum variant's payload. A type that is only ever written needs none, and the derive's examples no longer imply otherwise.
- **Where an error out of a declaration lands.** The same file promised that a field whose type has no `Read` impl is reported at that field. One macro call covers every field, so it is reported at the declaration: the struct's name under the derive, the whole invocation under a hand-written one. What does land where it was written is the derive's own refusals and an adapter named by `with = ".."`.
- **The same rule, in the README.** It read as though reaching for `read_into` lifted the `Default` requirement. It lifts it for the value handed in and for nothing beneath it, so `read_into(&mut Vec<T>, ..)` still asks `T` for one and no spelling of the read avoids it; `Box<T>` and `[T; N]` do escape it, having no element to build. And a tagged enum's payloads need `Default` where the enum itself does not, which the types table had as both. `tagged_enum!` now shows the all-payload shape that cannot derive one.

## [0.4.0] - 2026-09-04

### Changed

- **A raw identifier's `r#` is no longer part of its key.** A field or variant written `r#type` had the key `r#type`, because that is what `stringify!` hands the macro. It is now `type`, before any case rule runs, since the prefix is how Rust spells a name that collides with a keyword rather than part of the name: `r#type` is how you write a field for a `"type"` key. Both formats, fields and variants, derived and declared. An explicit `"r#type" => field` is a literal and is unchanged. **Breaking** for a declaration with a raw identifier and no explicit key, which now reads and writes a different key.

- **An internally tagged enum's tag no longer has to come first.** `tagged_enum!(.. as tag "kind")` used to refuse an object whose first member was not the tag with `ExpectedTag`, which refused every document from a sorted-key writer the moment a member sorted before the tag. The reader now steps over the members before the tag, dispatches on it, reads the members after it, and then reads the ones it stepped over, nesting as deep as the payloads do. A tag that is first still costs one pass; the members before a late tag are walked twice, and a key on both sides of the tag keeps its earlier value. Required-field and unknown-key rules apply to the deferred members as to any other. An object with no tag at all is still `ExpectedTag`, reported against its first key.

### Added

- **`#[derive(Structio)]`, behind the `derive` feature.** A front end to `object!`, `array!`, `unit_enum!` and `tagged_enum!`: it reads the type and emits the declaration, so a derived type and a declared type are the same impls. `rename_all`, `tag`, `array`, `element`, `json`, `beve` and `crate` on the type; `rename`, `skip`, `required` and `with` on a field; `rename` on a variant. Generics and their bounds are read off the type. The feature is off by default and the derive crate has no dependencies. [docs/derive.md](docs/derive.md) has the rest, including what later stages add.
- **BEVE containers reserve on the wire count.** `Reader::read_seq_counted` and `read_map_counted` hand the element count to the caller before the first element, clipped to what the input could hold, and `beve::cautious::<T>` clips it again to a megabyte of `T`. `Vec`, `VecDeque`, `HashMap` and `HashSet`, adapted or not, reserve once instead of doubling up; a hostile count can waste at most that megabyte.
- **`Value`, a tree for a value with no declared type.** Null, bool, number, string, array, object, with `get`, `pointer`/`pointer_mut`, the `as_*`/`is_*` accessors, `Index`/`IndexMut` by key or position, and the `value!` macro to build one. It reads and writes through both formats like any other type, so it can be a field of an `object!` declaration or a whole document; a BEVE typed array, complex run or matrix reads into the same shape `beve_to_json` writes. `Number` keeps whether it was an unsigned integer, a negative integer or a float, and writes a whole-valued float as `1.0` so the kind survives a trip through text. `to_value` and `from_value` move a declared type in and out, through JSON text. This is for the value nothing decodes, a register tree walked by path, not a substitute for a declared type, and the crate's stance on that is unchanged.

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

[Unreleased]: https://github.com/matrix-research-inc/structio/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/matrix-research-inc/structio/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/matrix-research-inc/structio/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/matrix-research-inc/structio/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/matrix-research-inc/structio/compare/v0.3.2...v0.4.0
[0.3.2]: https://github.com/matrix-research-inc/structio/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/matrix-research-inc/structio/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/matrix-research-inc/structio/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/matrix-research-inc/structio/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/matrix-research-inc/structio/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/matrix-research-inc/structio/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/matrix-research-inc/structio/releases/tag/v0.1.0
