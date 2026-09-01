# Correctness

A serializer is only worth its speed if the bytes are right. This is what is checked and how.

Run it with `cargo test --release`, plus the opt-in exhaustive `f32` sweep described below.

## Numbers

**Floats parse** identically to the standard library over 500k random bit patterns and 200k generated decimal strings, per width, including the classic `strtod` boundary cases.

**Floats write** the shortest decimal that round-trips, checked against the standard library's `{:e}` over 500k values per width. Where the exact value falls precisely between two decimals the choice is a convention: zmij rounds half to even and Rust's Grisu path does not always, so the tests assert that the digit *count* and exponent always match and that any difference is a single-digit tie resolved to even.

**The exhaustive sweep** checks all 2^32 `f32` bit patterns:

```sh
cargo test --release -- --ignored float_f32_exhaustive
```

Every one of the 4,278,190,078 finite non-zero values round-trips, and no decimal one digit shorter does, in either rounding direction. Takes about 7 minutes.

## Key lookup

**Perfect hashing** is checked for exactness over hand-picked key sets covering every scheme in the ladder, plus 400 generated key sets of up to 40 keys.

Both formats share one table but reach it through different entry points, since JSON has to find where a key ends and BEVE is told. Those two are asserted to resolve every key of every generated set to the same index, which is what guards against them drifting apart.

**A `#[required]` field's bit is the same index**, so the mask is checked against what the readers actually set rather than only against the declaration: one case per marked field in each format, from documents that carry every other member. The case worth the trouble is the wide struct. A mark needs a bit only for itself, so a struct of more than 64 fields may still have one, and the field past the 64th is where an implementation would go wrong quietly -- a shift by 64 wraps to bit 0 on most machines, which would credit a marked field for a member that is not there. A 70-field struct is read from a document holding only its 65th member and required to refuse it, in both formats.

## Round-tripping

**JSON** is fuzzed over 20k generated documents containing escapes, multi-byte UTF-8, astral-plane characters, and subnormal and extreme floats. Every prefix and single-byte corruption of 2300 documents is checked to produce an error rather than a panic.

**BEVE** is fuzzed over the same generated documents, with the two formats asserted to land on the same value. Every prefix and every single-byte corruption of those documents is checked to produce an error rather than a panic, as are 20k arbitrary byte strings read into four different destination types.

**Laying out text** is asserted against the writer rather than against fixtures: for every generated document, prettifying the compact form must give exactly what writing the value under that policy gives, under indented, inline-array and compact policies alike. Every prefix and single-byte corruption is checked to produce an error rather than a panic, and to be laid out unchanged in meaning whenever the reader still accepts it.

**Minifying** is asserted the same way, from the other end: for every generated document and every layout the writer can produce, minifying it must land exactly on the writer's compact form, and must agree byte for byte with what `prettify_with::<Standard>` makes of the same text. Prefixes and corruptions are checked to produce an error rather than a panic, never to grow a document, and to preserve its meaning whenever the reader still accepts it -- and, when the reader refused it, to leave it refused, since taking a layout out is not a repair.

Layouts this crate produced are the easy case, though, so a second property splices whitespace into *every* position of a compact document that could take it -- tabs, carriage returns, blank lines, a space before a comma -- and requires the original back exactly. A third does the same with `//` and `/* */` bodies, which is the only thing anywhere that exercises a cached whitespace run with a comment inside it.

## Enums

The generated documents carry both wire forms, so an enum is inside every property above rather than beside them: a variant name, a tag over a struct, a tag over a scalar, a tag over a string, and a run of tags in a sequence. Round-tripping, corruption, truncation, framing, transcoding, prettifying and minifying all meet them.

What is asserted separately is the part that is a *choice* rather than a consequence. Both forms are accepted for either kind of variant, so `"Empty"` and `{"Empty":null}` must land on the same value and `{"Empty":0}` must not. A variant that carries a value has no bare form, and says so with a distinct error rather than calling its own name unknown. A tag naming no variant is refused under every policy, `SkipUnknown` included, and the error points at the name rather than at the punctuation around it. A name that only *collides* with a real one reaches the confirmation step and fails it, which is what stops the perfect hash from accepting a key it never had.

That a run of unit variants is stored as a BEVE string array is asserted against `Vec<String>` rather than against written-out bytes, since being the same encoding is the whole claim, and the generic form is checked to still read back, so the two stay interchangeable.

Reading into a value that already holds the same variant is checked to keep that payload's allocation, since a reader that replaced it wholesale would return the same value and defeat the point of reading into a reference. The BEVE side is pinned on the bytes as well as through the reader: the object form is written out literally in a test, because no writer here produces it for a variant carrying nothing.

Four of the guarantees are compile-time, and none of them is in the suite, because asserting that something fails to build needs a second compilation and this crate has no dependency to run one. They hold by construction instead, and it is worth saying which construction. A `unit_enum!` whose variant carries a value has no rule in the macro that matches it. A `tagged_enum!` that leaves a variant out is refused by the compiler's own exhaustiveness check, which the generated `write` ends with a `match` over every declared variant to invoke. A variant with more than one field fails on the arity of the pattern the macro builds for it. And a declaration that names the same variant twice under two wire names is refused by a constant per name in a scope of its own, so the repeat is an `E0428`. All four were confirmed by hand against the messages they produce: the second names the variant that was left out, and the first is a `compile_error!` in as many words, since without one the failure is a `macro_rules!` matcher error pointed into this crate rather than at the declaration that caused it.

## The BEVE encoding

**Pinned to the specification** by golden byte vectors for every construct, written out literally rather than computed from the header helpers, so a change to those helpers cannot move the goalposts along with the code.

**Cross-checked in both directions against an independent implementation**: each reads the other's output for a document exercising nested objects, typed arrays, packed booleans, string arrays, and integer-keyed maps, and the two agree byte for byte. This is the check that turns "self-consistent" into "interoperable". The aligned form is checked in the direction that matters for a writer: the reference implementation's reader loads a document written with it and lands on the values it reads from the plain form. Neither check has a harness in this repository; both are run by hand against the other implementation.

**Measuring is checked against the bytes**, which is the only oracle it has. `beve::size` claims to report exactly what writing produces, so every value in `tests/size.rs` is measured under each policy this crate ships and one it does not, in the plain and aligned forms alike, and the answer must equal the length of the document actually written. The corpus is chosen for where a measurement could diverge rather than for coverage of the type list: the compressed size's four widths from either side of each threshold, packed booleans at every length up to forty, an aligned payload at every offset across two alignment periods, adapters that write something other than their type would, and objects whose member count `SKIP_NULL` makes conditional. The same equation is checked for a value that does not begin the document: what `beve_size_aligned_after` claims at a base offset is what appending to a buffer of that length adds, at every base across two alignment periods and for every element width, with the payload's landing place recomputed from the emitted bytes rather than asked of the writer a second time. Every value in the corpus is measured behind a prefix as well as at zero, under `SKIP_NULL` as well as the default, since that is the one policy constant the BEVE writer reads. A frame appended to a buffer that still holds earlier frames -- the one position a buffer's own length cannot imply -- is checked separately, against both the equation and the payload's landing place within its own frame. Separately, measuring a four-megabyte document is asserted to ask the allocator for nothing whatsoever, a measurement that staged the bytes somewhere being worse than the buffer it exists to replace.

## Walking without decoding

Validation and pointers walk the same headers reading does, so the property that matters for both is agreement with reading rather than self-consistency.

**Validation** is checked against reading over every prefix and every single-bit corruption of a document exercising each construct the writer emits: a prefix must be rejected by both, and a corruption that validates must never then fail to read for a structural reason. Anything the writer produces is asserted valid on every fuzz round.

**The nesting limit is checked in both directions**, at the boundary and for both container kinds. Charging a level too many would refuse a document reading accepts; charging one too few would let a validator pass input the parser then refuses, which is the worse of the two and the one a typed array is placed to catch. A typed array never recurses, so it looks like it should cost nothing, but reading charges it a level and so must every other walk.

**Transcoding** is checked against the typed path, which is the strongest oracle available for it: `beve_to_json(&to_beve(&value))` must be the bytes `to_string(&value)` produces, over the generated documents and over a hand-built one covering every construct. Separately, any document the validator accepts must transcode, unless it holds a value with no JSON form at all, and that is asserted over 20k arbitrary byte strings and a corruption at every byte position of a generated document. Widths no Rust type has, the aligned array form, and bytes no writer here emits are pinned by golden text.

**Pointers** are checked against parsing over the generated documents: every value reachable by parsing one whole must come back identical when reached directly, through each container kind, and one past the end of each must report absence rather than whatever follows it. Malformed pointers are checked to be reported as malformed whatever the document holds at that level, so a typo does not depend on the data to surface.

## Streaming

**Streaming writes** are compared byte for byte against the in-memory writer at every buffer size from one byte up, on documents whose strings are full of the braces, brackets, quotes and backslashes a drain could be confused by. Draining rewrites the buffer under a writer that is about to overwrite its own last byte, so every size is a distinct case.

**Streaming reads** are checked to recover the same values at every read size, including one byte at a time, across all three JSON framings and both BEVE ones, and a `Feed` is checked against every two-way split of a document. That a typed BEVE array streams element by element is checked at every stored width and for all four layouts, packed booleans included at every length from zero to forty, so the run that ends mid-byte is covered.

**That the window stays small** is asserted directly, because a reader that quietly buffered the whole file would pass every correctness test above.

**Equivalence with the batch path** is asserted as a property, not in prose: a streamed parse accepts exactly what `from_str` accepts, malformed inputs included, since the streaming side only frames and the ordinary parser decides.

**Framing** is checked to agree with `from_str` on whether a document is valid, over generated arrays with a structural byte inserted or a truncation applied. The splitter makes one grammar decision of its own, which positions may hold `]`, and this is what pins it.

**BEVE framing** is a fourth walk over the same headers, and the one that has to suspend part way through them, so it is checked the same two ways. Every document the validator accepts must frame as exactly one value and cut it at exactly its end, asserted over 20k arbitrary byte strings and a corruption at every byte position of a generated document; and every prefix of a document must fail to frame, since a value cannot be complete before its own bytes are. On top of that, one document per shape the walk has an arm for -- each scalar kind, both array forms, all four typed-array layouts including the aligned one, integer and string keys, and all four extensions -- is pushed a byte at a time and required to come back as one value.

**The two extensions that carry data** are checked at the two places they can go wrong. A complex array's class header is bit for bit the number header of the same class and width, so the properties pin that a `Vec<f64>` refuses a complex array and a `Vec<Complex<f64>>` refuses a numeric one, at a width where the synthetic element header collides with another extension's as well as at one where it does not. A matrix's extents and its data can disagree, which is a document nothing should have written, so the shape is checked on the way in and a failed read is required to leave the matrix empty rather than stating a shape it does not hold. Both types are in the generated documents the round-trip, corruption, truncation, framing and transcode properties all run over, so every walk in the crate meets them.

**The aligned form is checked as a layout, not as a feature.** Where the payload lands is the whole of the claim, so it is asserted on the bytes rather than through a reader, which would agree with a writer that ignored the padding: every array is written last in its document, at each width and at forty successive offsets, and its payload must start on a multiple of its element width, the empty array included. What must not change is asserted over the generated documents: an aligned document validates, reads back as the same value, re-serializes identically, and transcodes to the same JSON as the plain one. Sink writes are compared byte for byte with the in-memory ones from a one-byte buffer up, and for a block large enough to bypass the buffer, since a writer that measured padding from the buffer would go wrong at the first drain. That the form is used only where the specification defines it is pinned by requiring booleans, strings, one-byte elements, complex arrays and scalars to come out as the plain writer leaves them.

Two things about it are asserted because a correct-looking value would hide them. That the payload is still taken in **one copy** is asked of the bulk path directly, since a reader that walked an aligned array element by element would return the same values and defeat the point of the form. And floats are compared **by their bits**: an element-by-element read of an `f32` array converts through `f64`, which quiets a signalling NaN, so equality alone would not have noticed.

**One direction of the depth limit is deliberately loose, and it is worth knowing which.** `Vec::read` takes the bulk copy without going through `read_seq`, so it charges no level, where `validate` charges one for the typed array it copied. A document at exactly the limit can therefore read successfully and fail to validate. That is the safe direction -- the reader accepts a superset, never a subset -- but it does mean `from_beve(x).is_ok()` does not imply `validate_beve(x).is_ok()`, so the validator is a filter for input you have not read yet rather than a predicate on what reading will accept.

**The depth limit is checked to be the reader's**, not the splitter's own. That needs care to test at all: the reader gets a second opinion on the span it is handed and applies the limit again from zero, so a splitter that framed a document too deep would still produce an error, just not its own. The property therefore looks at how far the stream advanced rather than at whether an error appeared, and it wraps every container that charges a level in turn -- generic array, object, type tag, matrix -- around both a scalar and a typed array. A complex array is the one sequence that charges no level, since it holds numbers and cannot recurse, and that too is pinned in both directions: at exactly the limit a complex array still fits where a typed array does not.

## Unsafe code

There is a small amount, and all of it is checked under **Miri with strict provenance**. What may be reinterpreted as bytes, and back, is not prose: it is the `NumericBytes` bound, an unsafe trait carried by the fixed-width numbers and by `Complex` of one. See [design.md](design.md#three-soundness-notes).

That trait is implementable from outside the crate, which is what lets an adapter over a foreign scalar reach the same block paths, so the obligation it carries is checked from outside too: `tests/blocks.rs` implements it for a `#[repr(transparent)]` newtype and runs the whole adapter through Miri. One of its four clauses does not need Miri at all -- that an element is the width its declared header names is a constant of a generic type, so an impl that disagrees is a build error rather than a silent misparse, reported when the crate is *built* rather than by `cargo check`.

The blocks themselves:

- The writers' spare-capacity trick, writing into uninitialized capacity and then setting the length, over the whole test suite including every drain size.
- BEVE's bulk array copies, at all twelve integer and float widths, into both fresh and already-populated `Vec`s, and through an out-of-crate `NumericBytes` impl reached by an adapter.
- The borrow that hands a block back as a `&[T]` pointing into the document, at every offset a document can land on. Miri is the check that matters most here, since it hands out allocations at exactly the alignment that was asked for where a real allocator gives more, so it is where a missing alignment test shows up rather than hides.
- The key map's unaligned loads, over key sets constructed to force every one of the hash schemes.

The JSON drain is separately checked never to cut a character in half, since the retained tail is converted to a `String` without revalidation.

Miri interprets rather than executes, at hundreds of times the cost per operation, so under `cfg(miri)` the randomized tests draw a smaller sample, the buffer-size sweeps run over a shorter document, and the corruption sweeps step through byte positions rather than visiting every one. Every branch a round can take is reachable within the first few, and "every buffer size" stays literally true of the shorter document, so what the interpreter is there to see it still sees.

## Continuous integration

Every push runs the suite in both debug and release on Linux, macOS and Windows. Debug is not redundant: it is what checks the `debug_assert!`s guarding the numeric kernels, and release is what anyone actually runs. Alongside those, one job holds the line on lints, formatting and rustdoc, one builds against the MSRV exactly rather than against whatever is oldest still supported, and one builds the packaged tarball, since a file left out of a release cannot be put back.

**Big-endian** is covered by cross-compiling to s390x and running the suite under qemu. BEVE writes typed arrays as raw little-endian blocks and byte-swaps them elsewhere, and that second branch is unreachable on every machine anyone is likely to develop on.

**Miri and the exhaustive `f32` sweep run weekly** rather than per push, both being too slow for the inner loop. Miri is run through nextest, which gives each test a process of its own: it is single threaded per process, so the plain runner leaves every core but one idle, and the same suite takes about 20 minutes that way against about 100 sequentially. What bounds it now is a single test, `corrupting_any_beve_byte_never_panics`, at 1157 seconds of a 1213 second run, so more cores buy nothing and the only lever left is that test's own sample.

Running it weekly is a trade worth naming. Unsoundness reaches `main` unchecked for up to a week, where before it was caught on the push that introduced it. What makes that tolerable is that the unsafe here is small, changes rarely, and is bounded to a handful of places, so the window is nearly always empty; when one of those places is touched, the job is worth running by hand rather than waiting for Monday.

## Documentation

Code in a markdown file is not compiled, so it rots silently. The hand-written schema in [schema-declaration.md](schema-declaration.md) and the quickstart in the [README](../README.md) are both quoted verbatim from runnable examples, and `tests/docs.rs` fails if a doc and its example diverge.

## Generated tables

The parser's power-of-five table (`src/num/table.rs`, about 10 KB) is generated, not hand written: `python3 tools/gen_pow5.py` regenerates it byte for byte, asserting its output against known values first. The writer's power-of-ten table is the same size but is not checked in at all; `src/num/zmij.rs` rebuilds all 618 entries from 51 seed values in a `const` block at compile time.
