# Design notes

How structio is put together, and why. This assumes familiarity with Glaze; the interesting parts are where Rust forced a different answer to the same question.

## The shape of the thing

```
error.rs      ErrorCode (one byte), the located Error, and StreamError
keymap.rs     compile-time perfect hashing of object keys
traits.rs     Keys (the format-independent schema) and the all-formats ReadWrite bound
macros.rs     object!, json_object!, beve_object!
swar.rs       byte scanning
num/          atoi, atof (Eisel-Lemire), itoa, zmij, and their tables

stream.rs     the byte window both streaming readers pull through

json/         parser.rs, writer.rs, impls.rs, traits.rs
json/stream/  io::Read and io::Write, for documents that do not fit

beve/         header.rs, reader.rs, writer.rs, impls.rs, traits.rs
beve/stream/  the same, over binary

ext/          Complex and Matrix, the two BEVE extensions that carry data

transcode.rs  BEVE out as JSON, driving one format's writer from the other's reader
```

There is no intermediate representation anywhere. `from_str::<Config>` walks the bytes once and writes each value into its final member as it goes, the same as `glz::read_json`. `from_beve::<Config>` does the same over binary.

### What the two formats share, and what they do not

They share exactly one thing: `Keys`, the field list and its compile-time hash table. That is not an economy, it is the claim the library makes -- a struct's schema is a property of the struct, not of an encoding -- and `object!` emits it once.

Everything below that is per format: `json::Read`/`json::Write` against a `Parser`, `beve::Read`/`beve::Write` against a `Reader`. Making the traits generic over a format parameter was considered and rejected: it would put a type parameter into every user-written impl and every trait bound, to unify two bodies of code that have almost no shared statements. Two flat trait families cost some repetition in `impls.rs` and nothing at a call site.

The two writers are also separate types, for a reason stronger than taste. `json::Writer`'s buffer is always valid UTF-8 and always ends in a rewritable comma, which is what its drain policy is built around; `beve::Writer`'s holds arbitrary bytes and never rewrites one. A shared buffer parameterised by those two facts would put a `const TEXT: bool` through the crate's hottest file to save about eighty lines.

## Compile-time key hashing

This is the direct analogue of Glaze's `make_keys_info` / `decode_hash`, and it is the reason object parsing is fast. `KeyMap::build` is a `const fn`, so by the time the parser runs, mapping a key to a field index is a load and a mask.

The builder walks a ladder and stops at the first scheme that works:

| scheme | cost at runtime | when it applies |
|---|---|---|
| `SingleElement` | nothing | one field |
| `Mod4` / `XorMod4` / `MinusMod4` | one byte op | 3-4 fields whose first bytes land on `0..4` |
| `UniqueIndexTwo` | one byte compare | two fields |
| `UniqueIndex` | one table load | some byte column differs across every key |
| `FrontHash2/4/8` | one multiply, one load | leading 2, 4, or 8 bytes are distinct |
| `UniqueIndexSized` | quote scan, multiply, load | a byte column plus the key length |
| `UniquePerLength` | as above, column chosen per length | lengths separate the keys |
| `FullFlat` | hash of the whole key | anything else |
| `Linear` | compare each key | no seed found, or more than 255 keys |

The `255` bound is the `u8` bucket entry, not the practical limit. The bucket table is a fixed 256 slots, so a perfect hash of `n` keys succeeds with probability roughly `e^(-n^2/512)`: measured over random key sets, objects stay hashed through about 64 fields and fall to `Linear` by 80. Regular field names (a shared prefix with distinct suffixes) do much better and reach `FullFlat` well past that. Widening the bucket table independently of the per-length table would raise the floor, at a few kilobytes of read-only data per declared type.

`UniqueIndex` is the common case for real structs and is *exact*: it needs no seed search, because the byte column is distinguishing by construction. `{"testStrings", "testUints", "testDoubles", "testInts", "testBools"}` differ at position 4, so the whole lookup is `table[key[4]]`.

Every scheme is a **candidate generator**. The caller always confirms with a full key comparison, exactly as Glaze's `decode_index` does, so a collision from an unknown key can never select the wrong field. That confirmation lives in macro-generated code where the key is a literal, so `key.len()` is a constant and the comparison inlines to a fixed-size compare rather than a `memcmp` call.

Unfilled bucket slots hold the sentinel `n`, which is what makes rejecting an unknown key cheap: it usually lands on a sentinel and never reaches a string comparison at all.

### Where Rust forced a different answer

Glaze sizes each type's bucket table to `bit_ceil(N^2)/2`. Rust cannot size an associated const's array by another associated const without `generic_const_exprs`, so `KeyMap` is one concrete type with a fixed 256-slot table, and small key sets use only a prefix of it via `mask`. The cache footprint still scales with the object; only the unused rodata does not.

`Keys::MAP` is a `&'static KeyMap` rather than a `KeyMap`. `&` on a const expression promotes to an anonymous static, so the table lives in read-only memory. As a plain associated const it would be materialized onto the stack at every lookup site, which for a 544-byte struct is not a subtlety.

Duplicate keys and keys containing characters JSON must escape are rejected at compile time, by `panic!` in the `const fn`. Either would make a field permanently unreachable, and the failure would look like a missing key rather than a bug.

## The parser

Input is always `&str`, so the whole document is known to be valid UTF-8 before parsing starts. String values are sliced out and handed back with no revalidation, and unescaping only has to produce valid UTF-8 for the escapes it expands. This is a genuine advantage over the C++ version, which has to validate.

String scanning is SWAR: one pass over eight bytes at a time tests for `"`, `\`, and control characters together.

```rust
let q = chunk ^ QUOTE;
let s = chunk ^ SLASH;
let m = (q.wrapping_sub(ONES) & !q & HIGH)
      | (s.wrapping_sub(ONES) & !s & HIGH)
      | (chunk.wrapping_sub(SPACE) & !chunk & HIGH);
```

The quote scan for a key starts at `min_len`, since a shorter run cannot be a declared key. That is where Glaze's `quote_memchr` gets its head start too.

Bounds are checked. Glaze's default fast path assumes a NUL-terminated buffer, which removes a check from nearly every inner loop; a Rust `&str` gives no such guarantee, and the alternatives (a padded buffer type, or copying the input) cost either a doubled set of parser instantiations or a full copy of the document. The check is a compare against a value already in a register and predicts perfectly.

### What the parser accepts

The grammar is strict where JSON is strict. Trailing commas, leading zeros, `+1`, `.5`, `1.`, `NaN`, `Infinity`, unescaped control characters in strings, lone surrogates, non-string object keys, and trailing content after the top-level value are all rejected. Comments are the one relaxation a policy can ask for, and it has to be asked for: [`ALLOW_COMMENTS`](options.md#allow_comments) reads `//` and `/* */` wherever whitespace is allowed, and is off.

Four deliberate relaxations are worth stating outright, because none of them is visible from the error list:

- **Values under an unknown key are stepped over, not validated.** Skipping a number walks the token's alphabet rather than re-parsing it, and skipping a string does not interpret its escapes. `{"unknown":1.2.3e--,"real":1}` therefore parses under [`SkipUnknown`](options.md#error_on_unknown_keys). The cost of validating data nobody asked for is real; the benefit is not. Under the default the same document is refused at the key, before the junk is reached at all.
- **Duplicate keys are accepted, last one wins.** RFC 8259 leaves this to the implementation.
- **A key written with escapes never matches a field.** `{"\u0061":1}` does not fill the field named `a`; it is treated as unknown, and so refused or skipped by the [policy](options.md#error_on_unknown_keys) like any other unknown key. Key comparison works on the raw document bytes, which is what makes it fast, and `KeyMap::build` refuses at compile time to declare a key that would need escaping. Glaze behaves the same way.
- **Missing keys are silent by default.** A field absent from the document keeps whatever the value already held, which for `from_str` is its `Default`. [`RequireKeys`](options.md#error_on_missing_keys) is what makes it a `MissingKey` instead.

One asymmetry falls out of matching the standard library on parse: `1e400` reads as infinity, exactly as `str::parse` does, but infinity has no JSON form and the writer emits `null`. So that document does not survive a round trip.

### Numbers

Integers accumulate **unchecked**. Nineteen decimal digits always fit a `u64`, so overflow is decided once from the digit count rather than with a `checked_mul` per digit. This was worth 1.7x on integer-heavy documents: the per-digit branch chain, not the arithmetic, was the cost. Twenty or more digits fall to a cold out-of-line path that redoes the work with checked arithmetic.

The bulk loop is SWAR, eight digits per iteration, validated with two adds and a mask and folded with three multiplies.

Floats parse in three tiers: an exact path (mantissa at most 2^53, small exponent, one hardware multiply), Eisel-Lemire, and for the handful of inputs where 128 bits cannot resolve a tie, the standard library's big-integer path. The third tier effectively never runs, and deferring to `str::parse` there is both correct by construction and less code than a second big-decimal implementation.

`Parser::read_number_str`, which hands a number's text to a type this crate cannot convert to, walks the float scanner and discards the mantissa it accumulated. That costs a multiply and an add per digit against holding a second copy of the number grammar in step with the first, which is not a trade worth making for a path whose caller is about to run an arbitrary-precision parse.

## The writer

Members are written with an **unconditional trailing comma**, and the last one is overwritten with `}`. That removes the per-field "am I first" branch from the inner loop entirely. Each member's `"key":` prefix is assembled by `concat!` at compile time, so writing one is a copy of a constant string.

Under [`Options::PRETTY`](options.md) the comma is dropped rather than overwritten, so the closing bracket can go on its own line, and an empty container never wrote one and stays as `{}`. An array kept inline by `NEW_LINES_IN_ARRAYS` takes the overwrite instead, having no line to go to. The whole of that sits behind a constant, so a compact writer emits none of it and never touches the depth it would have read.

Short tokens go through `append_fixed`, which always copies a compile-time-constant number of bytes into reserved capacity and then sets the length to however many were real:

```rust
if v { self.append_fixed(b"true\0\0\0\0", 4) } else { self.append_fixed(b"false\0\0\0", 5) }
```

Selecting a slice by a runtime condition and extending by its length produces a `memcpy` call with a runtime length. Copying a constant eight bytes and adjusting the length instead took bool writing from 1293 to 6413 MB/s, past Glaze. The same trick applies to integers and floats.

Float digit generation splits the value into independent eight-digit chunks and each chunk into a tree of four divisions, so the multiplies that implement division-by-constant overlap instead of forming a chain.

### Prettifying is the writer, not a second layout

`prettify` lays out JSON text that arrived as text, and the interesting thing about it is that it contains no layout rules. It walks the input with the parser and emits its whitespace through the writer's own `open`, `close`, `line`, `colon` and `item`, which is the entire vocabulary the value path has. Prettified text is therefore byte-identical to what writing the same data under the same policy produces, and stays that way when a setting is added, because a setting is added in one place. The alternative -- a token scanner with its own idea of where a newline goes, which is how `glz::prettify_json` is built -- is a second copy of `NEW_LINES_IN_ARRAYS` waiting to disagree with the first.

Values are copied rather than re-encoded, so a number keeps the spelling the document gave it. Only the structure is checked, because only the structure has to be known to lay anything out; a token is stepped over by whatever finds its end, which for a number is its alphabet rather than the JSON grammar. So `prettify` lays out `{"a": 01}` unchanged.

That was a decision rather than an oversight, and it went the other way first. Holding the number to the grammar meant a second copy of it: reusing the reader's scan does not work, since the digit accumulation does not fall out when the value is dropped and forcing the call to inline costs the read path several times what it saves. Written separately and measured against the alphabet walk it cost 0-5%, worst on integer-heavy documents, and needed a fuzz property whose only job was to stop the two grammars drifting. What it bought was moving a rejection one step earlier than the reader that would make it anyway. Formatting is the job here and speed is the point, so the alphabet walk won, and `skip_value` and the prettifier now share one scalar skipper instead of having a strict and a lenient one each.

### Minifying is not the writer at all

`minify` goes the other way, and deliberately shares none of that. Laying a document out means knowing its shape, because which container a value sits in decides whether it gets a line or a space. Taking the layout away means knowing only where the strings are: whitespace inside one belongs to the document, whitespace outside one belongs to the formatter, and that is the whole of it. So the minifier counts no brackets, tracks no depth and reads no token. It copies runs of bytes through, stopping at a quote to step over a string and at whitespace to drop it.

That makes it lenient by construction rather than by choice, and it is worth being clear that this is not the prettifier's contract. `prettify_with::<Standard>` also compacts and agrees byte for byte on any document that is really JSON, but it walks the structure, so it costs more and rejects more. Two entry points, two contracts, and a fuzz property that they do not disagree on anything valid.

The one thing leniency must not buy is a changed document. Whitespace between two bare tokens is not the formatter's, it is the only thing holding them apart, and dropping it would turn `[1 2]` into `[12]`: well-formed, and not what came in. That is a single test against the byte before the run and the byte after it, using the same "could this be part of a number or a literal" predicate the stream splitter uses to find where a bare top-level value ends. Two other things are refused for the same reason -- there is no answer, not that the document is wrong: a string that never closes, and a slash that begins no comment where comments are whitespace.

## Enums are tagged by name

An enum's variants are named on the wire, never numbered. That is the whole of the encoding decision, and it is a decision about what a document means five years from now: an index binds a document to the declaration order it was written under, so inserting a variant in the middle silently changes every document already on disk. A name binds it to the variant, which is what the author meant. Glaze's `std::variant` support offers both, and BEVE's own type-tag extension is the numbered kind; it is deprecated, and nothing here uses it.

The form is external tagging: a variant carrying nothing is its name, and a variant carrying a value is that name used as the single key of an object. Internal tagging -- a `"type"` member beside the payload's own -- was not built, and the reason is structural rather than aesthetic. This crate reads in one pass with no lookahead and no buffering, and an internal tag can appear after the members it decides the meaning of, so reading one means either rewinding and re-reading the object or holding it somewhere first. External tagging puts the tag where a single pass can use it: first, and alone.

What the encoding buys beyond that is that a tag is not a special construct. It is an ordinary one-member object, so validating, pointing at a field, transcoding and framing walk through a variant with no knowledge of enums at all, and `/shape/Circle/radius` is a pointer like any other. The name goes through the same compile-time perfect hash a struct's keys do, so finding a variant costs what finding a field costs.

A variant carries at most one value, which is the shape `std::variant<A, B, C>` has. It also keeps the payload an ordinary type: the enum adds the tag and nothing else, so a payload declared with `object!` is written by the code that was already there rather than by a second schema mechanism for anonymous variant bodies.

### What the macro cannot do, and what it does instead

`macro_rules!` has no eager expansion: a macro cannot produce tokens for another macro's matcher, and `$(...)?` has no `else`. So a repetition cannot branch, and the two kinds of variant need different code in five places. The usual answer is a token-tree muncher, which recurses once per variant and so puts a ceiling on how many an enum may have; error enums are exactly the type that would hit it. So each of the five goes through a helper macro with one rule per kind, invoked from an ordinary repetition, and expansion stays linear. Writing is the one the formats share, the two calls being spelled the same in both, so only the pre-encoded key is passed in.

The cost is that a macro invocation cannot expand to a `match` arm, so `write` is a chain of `if let` rather than a `match` -- and a chain of `if let` is not checked for exhaustiveness, which would make a variant left out of the declaration a value that silently writes nothing. The generated body therefore ends with a `match` over every declared variant whose arms are empty. It is dead code that costs nothing and does one job: extending the enum without extending the declaration is a build error, and the compiler names the variant that was left out.

`unit_enum!`'s BEVE payload writer is the one place this does not apply. Its own matcher has already refused a variant that carries a value, so every arm has the same shape, nothing has to branch, and it is an ordinary `match` that gets the exhaustiveness check for free.

## A declaration that names a member twice

Every schema macro takes a list of Rust names, and nothing in the language stops the same name appearing in it twice. The key hash refuses a duplicate *key*, which covers the mistake whenever the names on the wire are the names in the code. It cannot see the other half: `"x" => f` beside `"z" => f` are two distinct keys naming one field, and a positional struct has no keys for it to check at all.

Left alone the failures are quiet ones. An `object!` field named twice writes its member twice, and reading takes whichever came last. An `array!` field named twice lengthens the array, so `[x, y, x]` is three elements and the third overwrites the first on the way back in. An enum variant named twice becomes an accidental alias, read under either name and written under the first. Every one of those compiles, runs, and produces a document that is well formed and wrong.

So each macro emits one constant per name in a scope of its own, and a repeat is `E0428`, "the name `f` is defined multiple times", pointed at the declaration.

It has to be an error rather than a lint, and that is the part worth writing down. Duplicating a `match` arm or a pattern binding would be diagnosed too, as `unreachable_patterns` or `E0025`. But a lint raised inside a macro expanded from *another* crate is suppressed, on the reasoning that the reader cannot act on a warning about code they did not write, and an explicit `#[deny]` in the macro body does not lift that. Measured: with the macro defined locally the duplicate arm warns; with the same macro behind a crate boundary, both the default lint and the explicit deny go silent. Only a hard error crosses.

## Two inlining decisions

These mattered more than any algorithmic change, and neither is obvious.

**Do not force-inline the generated field dispatch.** `read_field` holds the parser for every field of a struct. Marking it `#[inline(always)]`, which looks right because it is a dispatch, duplicates a whole nested struct's parser into each field arm of its parent, recursively. For the benchmark type (26 fields of structs of 5 fields) the entire parser collapsed into one function, and integer-heavy documents ran at **30% of their proper speed**. Changing four attributes from `inline(always)` to `inline` roughly doubled read throughput and tripled some write paths.

**But do not add an inline barrier either.** The obvious fix, `#[inline(never)]` on `read_object` and `write_object` to create a hard recursion boundary, measurably *hurt* writing. Left to `#[inline]`, LLVM makes better choices than either extreme.

## BEVE

BEVE is tagged and length-prefixed, so reading is a walk rather than a scan: a header byte says what a value is, and the bytes after it say how far it runs. There is no whitespace, no escape, and no delimiter to search for, which makes the reader considerably smaller than the JSON parser.

### The element header a typed array does not write

A typed array stores one header for its whole run, so its elements carry none. The obvious implementation gives every scalar reader a second entry point -- `read_u64` and `read_u64_with_header` -- and then has to keep the two in step forever.

Instead the array driver *installs* the header the next element would have had, and `head()` hands that out in place of consuming a byte:

```rust
fn head(&mut self) -> PResult<u8> {
    if let Some(h) = self.implied.take() { return Ok(h); }
    ...
}
```

Every scalar reader is then unchanged, and a packed boolean array works the same way: the driver computes each bit and installs a `true` or `false` header, moving the cursor not at all until the run is done. The payoff shows up in the error messages, which is how you can tell the abstraction is the right one -- a `Vec<String>` reading a boolean array says "expected a string", because the string reader genuinely saw a boolean header.

`take()` rather than a plain read, so the header is consumed exactly once. A nested value inside an element can then never inherit it, which is a class of bug that would only appear on malformed input.

### Bulk arrays without specialization

`Vec<f64>` should be a `memcpy` in both directions, and `Vec<MyStruct>` cannot be. Rust has no specialization, and `impl<T: Write> Write for Vec<T>` alongside `impl Write for Vec<f64>` is a coherence error, so the decision has to live on the *element* type:

```rust
trait Write {
    const ARRAY: Option<u8> = None;
    fn write_payload(items: &[Self], w: &mut Writer) where Self: Sized { unreachable!() }
}
```

`ARRAY` is the typed-array header a run of the type is stored under, or `None` for an element type that has no typed array and belongs in a generic one. Which typed array it is need not be spelled out in the type: the byte says it, and `write_payload` knows how to fill it. Being a constant, the match on it in `write_slice` folds at monomorphization and each element type keeps exactly one arm. Reading is the mirror image, except that it may decline: `read_bulk` takes the payload in one copy only when the stored element type is exactly this one and the host is little endian, and returning `false` costs nothing because the caller then drives the same array element by element. That fallback is what handles a `u8` array read into a `Vec<u64>`, which is a case real cross-language traffic produces constantly.

The bulk path takes the aligned form too. That form exists precisely so a reader can take the block whole, so a `try_bulk` that declined it -- which it did at first, because the aligned marker shares its category with booleans and strings -- would have made the fastest thing in the format the slowest thing to read, one element at a time, and would have done it silently: the values come back correct either way. What does not come back correct is an `f32` array's bits, since the element path converts through `f64` and that quiets a signalling NaN. The layout is the claim, so the test asks the bulk path directly rather than inspecting a value.

The aligned form rides the same constant. A payload that a reader means to point at rather than copy has to start on a multiple of its element width, which is a property of *where the array lands in the document*, not of what it holds, so it belongs to the writer and not to the type: `Writer::aligned` is a flag, `to_beve_aligned` is the writer that sets it, and every `Write` impl in the crate is unchanged. A wrapper type would have had to be threaded through the macros, both formats and `Matrix`'s data to reach the same places, and would still have left the padding depending on an offset only the writer can know. The flag is set at construction and read in one place, so for the entry points that fix it the branch folds away with the rest of the preamble and the plain path is the single store and `memcpy` it was.

The offset it pads against is the document's, not the buffer's: a sink writer drains as it goes, so what has already left is counted separately and the padding of a late array is measured against where that array will sit in the bytes the sink received.

And what the form is for, this crate now takes. `Reader::try_slice::<f64>` hands the block back as a `&[f64]` pointing into the document, which costs one comparison of the element header and one test of the payload's address. It has to be able to decline, because that address depends on where the document was allocated and `Vec<u8>` promises a single byte of alignment, so the type that carries a borrow into a struct is `Cow<'de, [f64]>`: borrowed where the document allows it, and the ordinary copy where it does not. That is also why there is no `Read` impl for `&'de [f64]` itself where there is one for `&'de [u8]`: a field that must borrow would make a program's correctness depend on the address its input happened to be allocated at, and a byte is the one width with no address to satisfy. Three forms are blocks -- a typed numeric array, its aligned form, and a run of complex numbers -- and the borrow and the bulk copy walk the preamble of all three through one function, so the two cannot come to disagree about which of them is worth taking whole.

Typed arrays are emitted only for contiguous sequences. A `VecDeque` or a `HashSet` has no single backing slice to be a payload, and gathering one would cost more than the compactness is worth, so those write generic arrays. Readers take either form for any sequence, so nothing depends on the choice.

### One hash table, two ways in

JSON has to find where a key ends; BEVE is told. `KeyMap::lookup_sized` is `lookup` with the quote scan removed and the length passed in, over the same table, seeds, and hash functions. The schemes that hash the key's leading bytes are unchanged, because those only ever read bytes inside the key: `min_len >= width` is a precondition of choosing them, which is also what makes the indexed loads in the sized version in bounds. The schemes that need a length get a better one than they could have found.

Two entry points sharing one table is a real risk -- they could disagree on some key and nothing would notice -- but they cannot here, because neither computes a hash the other does not. Merging them under a `const SIZED: bool` was considered and rejected: it would cost the JSON path nothing, since every branch on the parameter folds, but it would fuse two different in-bounds arguments into one. `lookup`'s leading-byte loads are in bounds because it checked the buffer's length; `lookup_sized`'s are in bounds because `min_len >= width` was a precondition of choosing the scheme. Sharing a body would make each `unsafe` block depend on whichever guard the const selected. The differential test over 400 generated key sets is the better guard against drift.

### Keys are encoded at compile time

A BEVE object key is `SIZE | DATA` with no header, and a struct's keys are literals, so both halves are known during const evaluation:

```rust
const N: usize = key_len(KEY);
const ENCODED: [u8; N] = encode_key::<N>(KEY);
```

Writing a member is then one copy of one constant array, which is the same trick the JSON side plays with `concat!("\"", key, "\":")`.

### Measuring is the writer with its stores removed

A frame header that states `body_length` has to be written before the body it describes, so the length has to be known before a byte of the body exists. Serializing into a buffer and taking its length answers that, but the buffer is exactly the copy `to_writer` exists to avoid: it is the one case where the streaming writer cannot be used at all.

The obvious answer is a `Size` trait beside `Write`, one method per type reporting what that type will occupy. That is two descriptions of one format, and the second is the one nobody exercises: it is right the day it is written and wrong the first time a header, a width, or a padding rule moves. A length that is one byte out is also the worst kind of wrong here, since the receiver cannot resynchronize from it.

So there is no second description. `beve::size` is the same writer with its stores taken out. Everything above the buffer -- the `Write` impls, the pre-encoded key constants, `count_fields`, the aligned form's padding -- runs exactly as it does when the bytes are kept; the six places the writer actually appends open with a test on a compile-time constant, and when it is set they add their own length to the offset counter and return. Nothing else in the crate knows this exists. A hand-written `Write` impl is covered without being asked, on one condition: that it positions itself from `offset` rather than from `len`. `len` is how much sits in the buffer, which a sink writer empties on every drain and a measuring one never fills, so a value laid out from it comes out differently under each of the three writers; `offset` is the position in the document and is correct under all of them. That is a real trap rather than a theoretical one, so reading the buffer while measuring is a debug assertion rather than a quiet zero.

The counter is `origin`, the field a sink writer already keeps so that padding is measured from the start of the *document* rather than of the buffer. That is not a coincidence worth arranging so much as one worth noticing: it means `offset()` answers correctly while measuring, and the aligned form's padding is computed rather than estimated.

It also means a document does not have to begin where the writer does. `origin` is bytes of the document that are not in the buffer, and a drain is only one way for bytes to get there: `Writer::at` is the other, and says that some other code wrote them. That is what a body behind a protocol header needs, since the padding that makes the aligned form worth writing is chosen from where the payload lands in the *frame*. Without it the offsets are counted from the body's own first byte, and the payloads land on their element width only where the header happens to be a multiple of 16 -- which is to say the feature works by luck or not at all. `append_beve_aligned` reads the offset off the buffer it appends to, which is right when the buffer is the message and cannot be got wrong there; `beve_size_aligned_after` takes it as an argument, having no buffer to read it from, which is the same reason the measurement exists at all.

That leaves the position the buffer cannot imply, and it is not a corner case: a send buffer accumulating frames back to back holds the earlier ones, so every frame after the first begins *inside* the buffer rather than at its start. `origin` is a `usize` counting bytes in front of the buffer and cannot say that, so `at` carries a second field, `skip`, for bytes at the front of the buffer that are none of this document's. Two fields for one number, because the number is signed and the wrapping arithmetic that would fake it in one field trades a documented invariant for a hidden one. `offset()` pays a subtraction it did not use to, which is measured in nothing: it is read twice, by the aligned form's padding and by the measurement, and by nothing on the plain write path. What it buys is that `at` is total -- every position a document can begin at is expressible, so there is no offset that has to be refused, and the write path stays free of panics.

The flag rides on `Options` because the writer's policy parameter is the only thing every `Write::write` signature already carries. A mode on `Writer` -- a second generic, or a const parameter -- would have to appear in every implementation's signature, in this crate and in anyone else's, to be reachable from a `Write` impl at all; a runtime field would put a branch in `push` and `raw`. So it is a hidden constant on `Options`, defaulting off, with the one policy that sets it private to the crate.

The cost of that choice is a list: `Measured<O>` forwards every constant of `Options` by hand, and nothing checks that it is complete. It is worth being exact about how weak that guarantee is, because it is tempting to believe the tests cover it. `SKIP_NULL` is the only constant the BEVE writer reads, so every other policy produces byte-identical binary, and deleting six of the seven forwards leaves the whole suite green. What would actually break is a *new* constant that reaches the writer and not the list, and the thing standing between that and a wrong frame length is a note at the foot of `Options`, where such a constant gets added. Generating the trait and the forwarding impl from one `macro_rules!` would remove the list entirely and was tried; it puts the crate's most-read public declaration inside a macro body, which was judged the worse trade for a file people read to learn what the options are.

What it buys is that most sizes are not walked at all. A member count, a key's bytes and a scalar's width are constants; a typed array is its count times its element width. So a struct of fixed-width fields optimizes to a single integer, and one holding a `String` and a `Vec<f64>` to a couple of loads and a few instructions with no loop and no data-dependent branch, however long the vector is. That depends on `header::encode_size` carrying `#[inline]`: it is not generic, so without the attribute a downstream crate compiled without LTO cannot inline it, and every size prefix becomes an out-of-line call with a constant argument. The attribute is what makes the paragraph above true off this repository's own release profile, and it pays on the ordinary write path too. What is left to walk is what genuinely varies: strings, generic arrays, maps, and the members `SKIP_NULL` may drop.

It is still a walk, which is why the guide recommends against it wherever the body *can* be buffered. Measuring and then writing is two passes where a reused buffer is one.

### Transcoding is the walk, not a second reader

`beve_to_json` is `skip_body` with the payload written out instead of stepped over. It recurses in the same places, charges depth against the same containers, and derives every extent from the same `byte_width`, `payload_len` and `typed_head`, which is why the reader's primitives are `pub(crate)` rather than private. Walks that each worked out for themselves where a value ends would eventually disagree, and the one that disagreed would be whichever was least used.

Two consequences fall out of that rather than being arranged. A document the validator accepts transcodes, unless it holds a value JSON has no form for at all -- a 128-bit float, or one of the two extensions that are not values -- or a matrix layout byte outside the two that are defined, which is asserted as a property rather than argued. And the output is not merely valid JSON but the *same* JSON the typed writer produces. That one does not follow from sharing anything -- the typed path goes through `Write` impls and `ToJsonKey` while the transcoder hand-rolls the writer's primitives -- which is why it is asserted rather than assumed.

There is no reverse direction. BEVE prefixes every object and array with its count, and JSON gives a container's size up only when it ends, so `json_to_beve` has to scan each container ahead, buffer its body, or write a size it patches later. All three are a different shape of program from this one, and the case for it is thin: JSON with a schema is already `from_str::<T>` and then `to_beve`, which gets typed arrays from the declaration instead of inferring them, and inference is the part a schemaless pump would have to invent a policy for.

### The extensions that carry data

`Complex` and `Matrix` live in `ext/` rather than in `beve/`, because a type is not a format. `object!` declares a struct for both formats at once, so a field of either has to work in both, and where BEVE has an extension JSON gets the encoding it would have had anyway: `[re,im]`, and `{"layout":…,"extents":[…],"value":[…]}`. Both types read those forms back out of BEVE too, so a producer that has no extensions is understood without a second declaration, and the transcoder now has an answer for both rather than a refusal.

Two things about the complex extension are worth recording, because both are traps.

The first is that a complex array's class header is *bit for bit* the number header of the same class and width. `0x61` is a complex array of `f64` and is also a lone `f64`; put another way, it is exactly what `element_of` yields for a typed `f64` array. The bulk path matches a stored element type against a header byte, so had that byte been used, a `Vec<f64>` would have taken the interleaved components of a complex array as plain numbers -- silently, at full speed, and with the right length. What it matches instead is `complex_element`, which is `element_of` with `TY_UNDEFINED` in place of the type: the class and the width stay where every reader already looks for them, and the result equals nothing else at all. The type field is three bits and the specification defines six values, so the seventh is the one code that can stand for something which is not a value, and using any *defined* code would have collided with the extension headers wherever the byte count is zero. That same byte is what the array driver installs for an element, the way a typed array installs a real one, and because it is unique nothing downstream has to ask where a header came from to know what it is. Read out of the input rather than installed it is an `InvalidHeader`, which is what the one `installed` test in `step` preserves.

The second is depth. A complex array is a sequence, and every other sequence charges a level, but this one charges none -- in `read_seq`, in `skip_value`, in the transcoder, and in the splitter. It holds numbers and nothing else, so no walk over one ever recurses, and what actually matters is that all four agree: charging it in one place and not another is how a validator comes to pass, at the very last level, a document the reader then refuses.

A matrix stores its data as an ordinary value, which is why `Matrix<Complex<f64>>` needed no case on either side: the extents are a typed array and the data is whichever array form the element type declares. It is also the type in this crate that cannot be allowed to hold a wrong value, since `Write` is infallible and there would be nowhere to report extents that disagree with the data. So the fields are private, `Matrix::new` checks the shape, `data_mut` hands out a slice rather than the `Vec`, and a failed read empties the matrix instead of leaving it half filled -- the one place where the crate's usual "partially written on failure" would be worse than nothing.

### What is not there

Skipping is where a reader is most easily wrong and least easily caught, because getting an extent wrong does not fail: it moves the cursor into the middle of a value and the *next* field is parsed from there. So where the specification leaves something undefined, this reader refuses rather than guesses. The complex header is the case that matters. Its number-or-array flag is three bits wide, so that the class and byte count line up with a number header, but only 0 and 1 are defined, and the two differ by whether a `SIZE` precedes the payload. Guessing at 2 through 7 would make the extent of a value depend on bits that carry no meaning, so they are an `InvalidHeader`. A matrix's layout byte is refused later rather than here, and deliberately: it threatens no extent, so `validate` has no reason to look at it, while `Matrix` and the transcoder both refuse a third value with `InvalidMatrixLayout` rather than name a layout and transpose the data. Aligned typed arrays are read, skipped, and driven as numbers, all three through one shared walk of the preamble, because those three have to agree on where such an array ends.

## Streaming

The streaming API deliberately adds no second parser and no second writer. `Documents` and `Feed` find where one value ends and hand that span to the ordinary `Parser` or `Reader`; `to_writer` is the ordinary `Writer` with somewhere to put its bytes. Two implementations of "what is valid JSON" would drift, and the streamed one would be the one nobody tested.

Both formats have the pair. What they share is `stream.rs`: the buffer, the growth and compaction policy, the size limit, and the offset bookkeeping that places an error against the whole stream rather than the current window. What they do not share is the search for a boundary, which is the `Framer` each side implements. That split is the same one the batch API makes -- one schema, two encodings -- applied to a buffer instead of a struct, and it means the fiddly half, including the one `set_len` in the crate's reading path, exists once.

### Framing is a scan, not a parse

`stream/split.rs` holds a scan that can stop at any byte and resume when more arrive. Its whole state is the nesting depth and whether it is inside a string, because nothing else can hide the byte that closes a value: numbers and the three literals contain no structural bytes at all. Strings are skipped eight bytes at a time with the same SWAR mask the parser uses, which also lights control characters; those are illegal inside a string but they are the parser's to reject, so the scan steps over them.

The consequence is that a chunk boundary can fall anywhere, including inside an escape, and nothing is re-examined when the next chunk lands. What the design does *not* do is deliver a half-filled struct. That would require a resumable state machine per type, which is where the second parser would come from. So memory is bounded by the largest single **value**, not the largest document, which for newline-delimited and array framings is one record.

Newline framing gets its own path: finding a boundary is a search for one byte, not a structural scan. That is sound for the reason the format exists, a literal newline being a control character that JSON forbids inside strings. It also resynchronizes, which the structural modes cannot: a corrupt record costs one line, where a mis-framed value elsewhere ends the stream.

The scan is supposed to make no grammar decisions, but it makes exactly one: in `Mode::Array` it decides which positions may hold `]`. Getting that wrong is invisible to any test that only checks well-formed input, and the first version accepted `[1,]` where `from_str` rejects it. The property that pins it is differential -- generated arrays, corrupted, with streamed acceptance asserted equal to `from_str`'s -- not a list of cases.

### BEVE framing is a walk, not a scan

BEVE states every extent up front, so its splitter looks at no payload byte at all: it reads headers, counts and object keys, and everything else is a number of bytes to step over. That sounds easier than JSON's scan and is harder to suspend. A scan can stop between any two bytes and resume from a few flags; a walk has to stop part way through a *stated* extent. So it carries two things across a chunk boundary: the containers it is inside, as an explicit stack, and how much of the current payload is still owed. Carrying the remainder rather than the whole extent is what keeps a value spread over a thousand chunks from being re-walked a thousand times.

A header and the sizes behind it can straddle a boundary too, and that one is solved by not writing a second header decoder. A throwaway `Reader` over the unconsumed bytes decodes the preamble; on success its own position says how far to advance, and on failure the walk's cursor has not moved, which is exactly the "read it and find it incomplete" semantics the resume needs. It also means a value's extent is worked out by the same code that reads and skips one. That matters more here than it looks: getting an extent wrong in a binary format does not fail, it moves the cursor into the middle of the next value.

`Mode::Array` covers typed arrays as well as generic ones, which needs one thing the batch path already had. A typed array's elements carry no headers, so a span cut out of one is not a value any `Read` impl could take. The splitter reports the header the array implied alongside the span and the reader is built with it installed, which is the same mechanism `read_seq` uses inside a typed array. Without it, streaming a file that is one enormous `Vec<f64>` would be impossible, which is the case BEVE most needs it for.

The depth limit is charged exactly as `skip_value` charges it, typed arrays included. The reader gets a second opinion -- it walks the span it is handed and applies the limit again from zero -- so a splitter that framed something too deep would still produce an error, just not its own, and the test that pins this has to look at how far the stream advanced rather than at whether an error appeared.

### Draining is free because it rides a check that was already there

The obvious way to drain a writer into a sink is to test the buffer length at member and element boundaries. Measured, that costs **8% on `mixed` writes, 15% on `bools`**: at six gigabytes a second the per-element body is a handful of instructions and one more compare is not noise. Hoisting the test out of the element loop fixes `bools` and costs the integer paths more than it saves, because duplicating the loop body changes what gets inlined into it.

The fix is to add no test at all. Every append already asks "is there room", so `Writer` keeps a `limit` that answers both questions at once:

```rust
fn room(&mut self, n: usize) {
    if self.buf.len() + n > self.limit { self.spill(n); }   // #[cold]
}
```

Without a sink `limit` is exactly `buf.capacity()`, so this is the capacity test `Vec` performed anyway and the batch path runs the instructions it ran before streaming existed. With a sink it is the lesser of the capacity and the drain threshold, and the cold path empties the buffer before considering an allocation. Draining then happens at every append point rather than at hand-chosen ones, which is both a tighter memory bound and less to get wrong. Writer throughput is within noise of the non-streaming version on all seven benchmark documents.

The cost is that `push` and the byte-run append now write into spare capacity and `set_len`, as `append_fixed` already did. That is checked under Miri with strict provenance at every buffer size from one byte up.

### The retained byte

Members are written with an unconditional trailing comma that is later overwritten with `}`. A drain must therefore never flush the buffer's last byte, since that byte may be a comma awaiting its rewrite. Draining keeps exactly one byte back, which is sufficient: the comma is always the most recent byte written when the rewrite happens, and appends after a drain only push the retained byte further from the end.

The indented form pops that byte instead of overwriting it, and relies on the same rule for the same reason. Which of the two a container takes is decided by whether it puts its contents on lines of their own, so an array written inline under `NEW_LINES_IN_ARRAYS = false` overwrites even though the document around it is indented.

## Three soundness notes

Three functions reinterpret memory: the two bulk block helpers and the borrow. What may be reinterpreted was once prose under a `# Safety` heading, held up by the discipline that only two macros ever called it; it is now the `NumericBytes` bound. That is an unsafe trait, sealed, implemented for the fourteen fixed-width primitives and for `Complex<T>` at the twelve widths BEVE has a class header for, and implementing it asserts no padding, every bit pattern a value, and that the little-endian bytes are the wire form for the element header it declares. The last of those is the part a compiler can check, and does: a `const` assert beside each implementation pins the width BEVE gives that header to the type's own size, so a payload of `n` elements is `n * size_of::<T>()` bytes by construction rather than by inspection. The half of the old contract the bound cannot carry is the document's half, that the stored element header is this type's own. That one is not a soundness matter -- taking a payload of the wrong width leaves the cursor inside the next value, which is a misparse -- so it stays a `# Correctness` note and a test each caller writes.

The borrow adds the one thing a copy never needed: the payload has to be on an address `&[T]` can point at. That is a runtime test rather than an argument, because the answer is a property of the allocation rather than of the code, and it is why `try_slice` returns an `Option` instead of taking the alignment on faith.

`Writer::into_string` converts with `from_utf8_unchecked`, because validating the output would be an O(n) pass on every serialization for an invariant that holds by construction. Keeping it *actually* by construction means safe code must not be able to append a non-ASCII byte, so `Writer::push` asserts. At every internal call site the argument is a literal, so the check folds away; only a hand-written `Write` impl passing a runtime byte pays for it.

String values are handed back as subslices of the input with no UTF-8 validation. That is sound only because the entry points take `&str`, and `from_slice` validates once up front. Slices are cut at `"` and `\`, both ASCII, so they always start and end on char boundaries.

## Known gaps

**Float writing was the one real gap.** It used to run Ryu, at 26-39% of Glaze. `num/zmij.rs` is a port of the algorithm Glaze itself uses, which roughly doubled float write throughput and moved it to 66-71% of Glaze. That is now in line with the rest of the library rather than an outlier.

The idea worth carrying: Ryu brackets the value in base ten and divides digits away until the interval stops admitting an answer, which is a data-dependent chain of 64-bit divisions. It is slowest on exactly the values real documents are full of, since an exact short decimal like `5.0` arrives with fifteen redundant digits. Zmij instead produces a fixed-width significand from one 128x64 multiply, holds one extra digit back, and lets the rounding test say whether that digit is needed. Trailing zeros then fall out of a leading-zeros count over the digit bytes rather than a loop, so the cost is the same for every input.

Digit formatting builds the output with fixed-width block copies rather than runtime-length ones, for the same reason `append_fixed` exists.

What is left on the table: Glaze's copy of zmij splits digits with SSE/NEON intrinsics and assembles the output through a table of precomputed field positions. This port takes zmij's portable scalar path for both, because the crate carries no architecture-specific code. Whether that accounts for the remaining gap has not been measured.

**No `json_to_beve`.** See above. `beve_to_json` covers the direction that needs no lookahead.

**Third-party types need an adapter or a wrapper.** Rust's orphan rule. A per-field [adapter](schemas.md#types-you-do-not-own) covers most of it without deforming the struct; a newtype is still needed when the foreign type has no `Default`. See the README.

**The benchmark is sensitive to code layout, and by more than it is to most real changes.** Adding an unused function to `keymap.rs` moves `strings` write by several percent, and a dead padding field in an unrelated struct has moved `bools` read by 15%. Splitting the crate into `json/` and `beve/` cost about 6.5% on `strings`, read and write, with the hot paths byte-identical either side; nothing else moved. Before believing a regression, check whether the source of the path in question actually changed, and take seven samples rather than three -- run-to-run spread on `strings` write alone is around 8%.

**Bucket tables are a fixed 256 slots.** See above. A `generic_const_exprs` Rust would size them per type.
