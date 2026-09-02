# BEVE

[BEVE](https://github.com/stephenberry/beve) is a binary format that keeps everything making JSON workable between programs that do not share a schema. Values are tagged, objects are keyed, and a document describes itself well enough to be skipped through without knowing what it holds.

What it drops is the text. A number is stored as a number at its own width. A string is stored with a length instead of a terminator and escapes. A run of numbers is stored as one header and one block of bytes.

The same struct declaration serves both formats:

```rust
#[derive(Default, Debug, PartialEq)]
struct Reading {
    sensor: String,
    samples: Vec<f64>,
}

structio::object!(Reading { sensor, samples });

let bytes = structio::to_beve(&reading);
let back: Reading = structio::from_beve(&bytes)?;
```

## Where the wins are

**Numeric arrays are a `memcpy`.** A `Vec<f64>` is one header, one count, and the slice's own bytes. Writing it copies the slice; reading it back into a `Vec<f64>` copies it back. The same holds for every fixed-width numeric type, for `Vec<bool>` (packed one per bit), and for `Vec<String>` and a `Vec` of any [`unit_enum!`](schemas.md#enums) type (a length per element, no per-element header). This is the single largest difference from JSON and the reason the format exists.

**Strings and byte runs borrow.** `&'de str` and `&'de [u8]` point straight into the input buffer with no copy at all, because BEVE stores both verbatim. Unlike JSON there is no escaping to undo, so this always works rather than only sometimes.

**Integer map keys stay integers.** A `HashMap<u32, _>` stores real integers at their own width rather than stringifying and parsing back.

**Non-finite floats survive.** `f64::NAN` and the infinities round-trip exactly. JSON has no representation for them, so the JSON writer emits `null`.

**Documents are smaller.** How much depends entirely on the data; numeric arrays compact the most and text the least. `examples/beve.rs` prints the ratio for its own sample document.

## Reading is lenient about width, strict about kind

A producer writes a number at the width its own type had, so a `u16` field here will meet a `u8`, a `u32`, or an `i64` on the wire depending on what the other side declared. Refusing those would make the format useless across languages.

So **any integer header satisfies any integer field**, with the value range-checked into the target, and any number header satisfies a float field. A value that does not fit is `NumberOutOfRange`, not a truncation.

What is *not* accepted is a different **kind**. A string where a number was asked for is an error, never a conversion.

Sequences accept a typed array or a generic one, whichever the producer chose, and a typed array of one width read into a `Vec` of another is widened element by element.

The same holds for a struct declared with `array!`, which reads either form: a three-`f64` struct takes the typed array another implementation wrote for its `[f64; 3]`. Which form it *writes* is up to the declaration. Name the element type and it is stored as a typed array, byte for byte what a slice of that type would have produced:

```rust
structio::array!(Rgb [u8; r, g, b]);   // 5 bytes, against 8 for a generic array
```

That is the interoperable choice as well as the compact one, since a fixed-size numeric struct is what the other side is likely to have written as an array in the first place.

## Reaching one field without decoding the rest

Every value states its own extent, so getting to a field costs a walk over the headers in front of it rather than a parse of the values in front of it. `from_beve_at` takes a [JSON Pointer](https://www.rfc-editor.org/rfc/rfc6901) and reads only what it names:

```rust
let port: u16 = structio::from_beve_at(&bytes, "/servers/1/port")?;
```

Everything off the path is stepped over whole, and nothing off the path is allocated for. A typed array is not stepped over at all: an element of a million-sample `Vec<f64>` is found by multiplying, not by walking. `read_beve_into_at` is the same thing into a value you already own, for pulling the same field out of document after document.

The pointer syntax is the standard one. `/` separates levels, `~1` spells a `/` inside a key and `~0` a `~`, and the empty pointer names the whole document. BEVE objects can have integer keys, which JSON Pointer was not written for, so a token that is an integer indexes those.

Two failures are kept apart. A well-formed pointer that names nothing the document holds is `NoSuchValue`; a pointer that is not well formed at all, such as one that does not begin with `/` or an array index spelled `01`, is `InvalidPointer`. Only the first of those is the document's fault.

The bytes after the value named are never looked at, so unlike `from_beve` this does not require the document to end where the value does. If that matters, validate first.

## Checking a document without decoding it

`validate_beve` walks a document and confirms every header, every length, every nested value, and every string's UTF-8, without turning any of it into a Rust type and without allocating:

```rust
structio::validate_beve(&bytes)?;
```

It is one pass over the bytes and no memory, whatever the document holds. Well formed means exactly one value with no trailing bytes, so a run of delimiter-separated values is several documents rather than one and is reported as trailing content. `beve::validate_reader` is the same over an `io::Read`.

Validity is a property of the bytes rather than of any type you have declared, so this says nothing about whether some `T` can read them. It is for input from somewhere you do not trust, and for telling a corrupted document apart from one that simply does not match your schema.

One thing a valid document can carry that this still refuses is an extension beyond the four the specification defines. Its extent is unknown, so the bytes after it cannot be located and the rest of the document cannot be checked at all; that is `UnsupportedFeature` rather than one of the malformed-document codes.

## Reading a document you have no type for

Everything above wants a declared type, which is fine right up until someone hands you a file and asks what is in it. `beve_to_json` answers that without one:

```rust
println!("{}", structio::beve_to_json(&bytes)?);
```

BEVE states every value's kind and its extent, which is exactly what a JSON writer has to be told, so the walk that reads the binary drives the text directly. There is no tree and no intermediate value: each input byte is read once and the only allocation is the output. `beve_to_json_writer` sends it to an `io::Write` as it is produced, so the text never has to exist all at once, and `beve_to_json_into` reuses a `String` when dumping one document after another. The input is a slice either way: a BEVE value is found by stepping over its neighbours, so it has to be there to step over.

What comes out is not an approximation of what the typed path would have written, it is the same bytes. `beve_to_json(&to_beve(&value))` and `to_string(&value)` are asserted equal over the generated documents, which is what keeps a third walk over the headers from drifting away from the two that were already there.

Where the formats disagree, JSON is the smaller of them:

- **Integer keys are quoted.** JSON has no other kind of key, so `1` becomes `"1"`, which is the form a `HashMap<u32, _>` is written as anyway.
- **Non-finite floats become `null`**, as they do everywhere else the JSON writer meets one.
- **Two of the four extensions are refused**, with `UnsupportedFeature`. A complex number becomes `[re,im]` and a matrix becomes `{"layout":…,"extents":[…],"value":[…]}`, which are not encodings chosen here: they are what `Complex` and `Matrix` write in JSON and read back from BEVE, so a document that goes through a transcode still loads into the same types. The delimiter separates documents rather than being one, and the deprecated type tag names a variant by an index whose meaning is not in the document: neither has a JSON form to take, and those two are the refusals.
- **128-bit floats are refused**, having no Rust type to widen through.

There is no `json_to_beve`. BEVE prefixes every object and array with its count and JSON gives a container's size up only at its end, so a pump in that direction has to scan ahead, buffer, or patch, none of which is the single straight walk this is. It is also mostly unnecessary: JSON *with* a schema is `from_str::<T>` and then `to_beve`, which takes typed arrays from the declaration rather than guessing them from the data.

## Reading a file that does not fit

`from_beve` wants the whole document, and `from_beve_reader` only hides the `read_to_end` that puts it there. For input too large to hold, or not yet fully arrived, `beve::Documents` hands out one value at a time:

```rust
let file = std::io::BufReader::new(std::fs::File::open("samples.beve")?);
let mut docs = structio::beve::Documents::array(file);
for sample in docs.iter::<Sample>() {
    handle(sample?);
}
```

`array` streams the elements of one top-level array, `values` streams whole documents written back to back, and `beve::Feed` is the same machine for bytes pushed at you rather than pulled. Memory is bounded by the largest single *value*, not by the size of the file.

Typed arrays stream too, which is the part BEVE makes interesting: a file that is one enormous `Vec<f64>` comes back as `f64`s, because the splitter supplies the header the array implied for each element. See [streaming.md](streaming.md#beve).

One whole value is the floor, though, and a document that *is* one enormous numeric or complex array has no smaller unit to be handed out in. `read_beve_array_into` is that case:

```rust
let file = std::io::BufReader::new(std::fs::File::open("samples.beve")?);
let mut samples: Vec<f64> = Vec::new();
structio::read_beve_array_into(&mut samples, file)?;
```

The payload goes from the reader into the vector's own memory, so the vector is the only thing held rather than the vector plus the document, and the block copy `Documents::array` gives up is kept. A complex array reads the same way, into a `Vec<Complex<T>>`. The price is that it is exact where the rest of the crate is lenient: the stored element type has to be `T`'s, and a stored `f32` read as `f64` is `ElementTypeMismatch` rather than a conversion. That case is what `Documents::array` is for, converting an element at a time under the same memory bound. A count read off the wire is never reserved on its word, so there is no size limit to set here as there is on `Documents`.

## Length-prefixed frames

A BEVE document states its own extent, so it needs no framing to be read back from a buffer, and `beve::Documents::values` will pick documents out of a bare run of them for exactly that reason. A length prefix is still worth writing when the frame has to be legible to something that does not parse BEVE -- a router, a length-limited transport, a log format that interleaves other records.

```rust
fn send_frames(values: &[Run], sink: &mut impl std::io::Write) -> std::io::Result<()> {
    let mut body = Vec::new();
    for value in values {
        // Clears the buffer and keeps its allocation, so after an iteration or
        // two this loop stops allocating altogether.
        structio::write_beve_into(value, &mut body);
        sink.write_all(&(body.len() as u32).to_le_bytes())?;
        sink.write_all(&body)?;
    }
    Ok(())
}

fn stream_frames(values: &[Run], sink: &mut impl std::io::Write) -> std::io::Result<()> {
    for value in values {
        // Exactly what the write below will emit, so the length can go out in
        // front of a body that never exists in memory at all.
        sink.write_all(&(structio::beve_size(value) as u32).to_le_bytes())?;
        structio::to_beve_writer(value, &mut *sink)?;
    }
    Ok(())
}

fn frame_aligned(query: &str, samples: &[f64]) -> Vec<u8> {
    let mut frame = vec![0u8; 8]; // room for the length
    frame.extend_from_slice(query.as_bytes());

    // The body does not begin the document, and the aligned form's padding is
    // chosen from where each payload lands, so both halves are told what stands
    // in front of them. Measured at zero, this length would be wrong.
    let body = structio::beve_size_aligned_after(samples, frame.len());
    frame[..8].copy_from_slice(&(body as u64).to_le_bytes());
    structio::append_beve_aligned(samples, &mut frame);

    frame
}
```

The first two produce the same stream. **Prefer the first.** It serializes once, where measuring first and then writing walks the value twice, and the second walk is the one that produces the bytes either way. Preallocation is already handled by reusing the buffer, and a size limit is cheaper to check on `body.len()` once the bytes exist than to compute separately beforehand.

**Reach for the second when the length has to reach the wire before the bytes do.** That is the case a fixed-width protocol header creates: the header states `body_length` and is written first, so the body cannot be staged in a buffer and measured afterwards without holding the whole of it. `beve::size` answers what the body will be without producing any of it, which is what lets the body go straight from the value to the socket.

**The third is the same frame with an aligned body.** Padding is measured from the start of the document, so a body sitting behind a header has to be measured and written knowing what stands in front of it: `beve_size_aligned_after` takes that offset, and `append_beve_aligned` reads it off the buffer it is appending to. Measured at zero and written behind a prefix, the two disagree by as much as fifteen bytes an array, and that difference is a `body_length` the far end cannot use.

A fixed header on its own will not show you this, which is what makes it worth stating. Forty-eight bytes is a multiple of every element width BEVE has, so a 48-byte header moves no padding at all and measuring at zero happens to be right; it is the variable-length part behind it -- the route above -- that moves the payload off its width, and it moves it differently for every route. `beve::Writer::at` says the same thing to a writer draining into a socket, where there is no buffer to read the offset from, and to one appending a frame to a send buffer that still holds the frames before it, where the buffer's length is the wrong answer rather than no answer.

### Why the two agree

`beve::size` is the writer with its stores taken out. The same `Write` implementations, driven through the same methods, with every header, count, key and padding byte decided by the same lines that decide them when the bytes are kept; an append adds its own length to a counter instead of copying anything. There is no second description of the format to fall out of step with the first.

A hand-written `Write` impl is carried along with everything else, on one condition: that it decides nothing from how much has been *buffered*. `Writer::len` and `Writer::as_bytes` report the buffer, which a sink writer empties on every drain and a measuring one never fills; `Writer::offset` is the position in the document and is correct under all three. Reading the buffer while measuring trips a debug assertion rather than quietly returning zero.

The policy has to be the one you will write under, since `SKIP_NULL` changes how many members an object holds. So does the form, and so does where the value lands. Hence `beve::size_with`, `beve::size_aligned` and `beve::size_after`, one for each, exactly as `beve::to_vec` has `to_vec_with` and `to_vec_aligned`.

### What it costs

No allocation, and not one byte of output touched. Sizes that are constants stay constants, so a struct of fixed-width fields measures as a single integer with nothing walked at all, and a typed array is its count times its element width however long it is. What remains to walk is the part whose size genuinely depends on the data: strings, generic arrays, maps, and members `SKIP_NULL` may drop.

## Interoperability

The encoding is pinned to the specification by golden byte vectors in `tests/beve.rs`, and cross-checked in both directions against an independent implementation: each reads the other's output for a document exercising nested objects, typed arrays, packed booleans, string arrays, and integer-keyed maps, and the two agree byte for byte.

One deliberate difference is worth knowing if you compare bytes with a `serde`-driven implementation. **An empty sequence keeps its element type here.** An empty `Vec<f64>` is written as an empty *typed* array, where an implementation driven by `serde` cannot know the element type with no elements to inspect and writes an empty *generic* array instead. Both forms are valid and both are read by either side; only the bytes differ.

## Undefined encodings are refused, not guessed

Getting a value's extent wrong in a binary format does not fail loudly. It moves the cursor into the middle of a value, and the *next* field is parsed from there. So where the specification leaves something undefined, this reader errors rather than picking an interpretation.

The complex extension's header is the case that matters. Its number-or-array flag is three bits wide, so that the class and byte count line up with an ordinary number header, but only two values are defined, and they differ by whether a size precedes the payload. Values 2 through 7 are an `InvalidHeader`.

A matrix's layout byte is the other one, and it is refused later rather than earlier. Its two defined values say which index varies fastest, and reading it wrongly transposes the data without changing any extent, so `Matrix` and the transcode both refuse a third value with `InvalidMatrixLayout` while `validate_beve` has no reason to look at it.

## Enums are tagged by name

A variant carrying nothing is a string, and a variant carrying a value is an object of one member keyed by the name. Both are core constructs, so nothing about an enum is BEVE-specific except how a run of them is stored: because a [`unit_enum!`](schemas.md#enums) value can only ever be a string, a `Vec` of one is a **string array**, one header for the whole run, and comes out byte for byte what a `Vec<String>` of the same names would. A `tagged_enum!` cannot have that, a variant carrying a value being an object, so its runs are generic arrays. Reading takes either form for either macro, so the two stay interchangeable.

**The type tag extension is deliberately not used.** It is the one construct BEVE offers for this, and it is deprecated; it also tags by index, which binds a document to the declaration order it was written under, so inserting a variant in the middle silently changes every document already on disk. A name binds to the variant instead. A document that carries a type tag is not rejected out of hand, since the extension states its own extent: `validate_beve` accepts it and a reader steps over it wherever a value may be skipped. What nothing does is give it a meaning. No Rust type reads one, and `beve_to_json` refuses it with `UnsupportedFeature` rather than inventing a JSON form for it.

Because a tag is an ordinary one-member object rather than a construct of its own, everything that walks a document without decoding it reaches straight through a variant. `validate_beve` checks one as the object it is, `beve_to_json` transcodes it with no case for enums anywhere in the walk, and a JSON Pointer descends through the name: `/shape/Circle/radius` resolves exactly as `/shape/radius` would if the field were a plain struct. That is the whole reason for the encoding, and it is what a complex array and a matrix give up by being extensions.

## Complex numbers and matrices

The two extensions that carry data have types: `Complex<T>` and `Matrix<T>`, with `MatrixRef<'a, T>` for writing a matrix whose data you already hold somewhere else.

```rust
use structio::{Complex, Matrix, MatrixLayout};

let signal = vec![Complex::new(1.0f64, 2.0), Complex::new(3.0, -4.0)];
let bytes = structio::to_beve(&signal);

let m = Matrix::new(MatrixLayout::RowMajor, vec![2, 3], (0..6).collect())?;
let bytes = structio::to_beve(&m);
```

A run of complex numbers is one header, one count, and the interleaved components, so a `Vec<Complex<f64>>` is a single `memcpy` in each direction exactly as a `Vec<f64>` is. No wrapper type is needed to get that: the run form is what the element type declares, so a `Vec`, a slice, an array and a struct declared with `array!` all take it.

A matrix stores its data as an ordinary value, which is what lets `Matrix<Complex<f64>>` work with no case of its own on either side: the extents are a typed array, and the data is whichever array form the element type has.

Both types read the form a producer without the extensions would have written, so no second declaration is needed to talk to one: a complex number reads back from a two-element array, and a matrix from an object holding `layout`, `extents` and `value`. Those are also the forms both write in JSON, and the forms `beve_to_json` produces.

`Matrix` cannot hold a shape it does not have. Writing is infallible everywhere in this crate, which leaves nowhere to report extents that disagree with the data, so `Matrix::new` checks instead and the fields are private; `data_mut` hands out a slice, so the values can change and their number cannot. A read that fails resets the matrix, layout included, rather than leaving part of a document behind.

Two things a complex array is not, despite being a sequence to `from_beve` and to `beve::Documents::array`. A JSON Pointer cannot descend into one, or into a matrix: an extension's insides are not addressable, so `/signal/1` is `NoSuchValue` where `/samples/1` resolves. And neither type is reachable through `beve_to_json` as anything but the JSON encoding above.

`Complex<T>` is a pair with no arithmetic on it. It exists so a complex number can be stored, and deliberately does not compete with `num-complex`: converting at the edge from whatever type your program computes in is a field-by-field move that the optimizer removes.

## Arrays a reader can point at

A typed array's payload starts wherever its header and count leave off, which is almost never a multiple of eight, so a reader that wants a `&[f64]` out of the document has to copy the block somewhere aligned first. BEVE's aligned form exists for that: the element type is stated in a second header and a run of padding stands in front of the payload, sized so that the payload begins on a multiple of its element width counted from the start of the document. `to_beve_aligned` writes it, and `beve::Writer::aligned` is the same switch on a writer you build yourself, which is how a sink or a reused buffer reaches it.

The same document, laid out differently. Every reader here takes either form, so nothing on the way back has to know which one it was given, and what it costs is the padding plus a form a decoder is less likely to have implemented. Only numeric arrays wider than one byte change: booleans and strings have no aligned form, one-byte elements are aligned wherever they land, and a complex array is an extension rather than a typed array. A document with no numeric array wider than a byte in it comes out byte for byte the same as `to_beve` would have written it. A matrix gets it for free, since its data is an ordinary typed array.

The offsets are counted from the start of the document, and a writer told nothing takes that to be its own first byte. For a document that will reach its reader behind a prefix -- a protocol header, a routing path -- that is the wrong zero, and the payloads land on their element width only if that prefix is a multiple of 16, 16 being the widest element BEVE has; a narrower multiple is enough for a document whose own widest array is narrower. Say what is in front of it instead: `append_beve_aligned` pads against the buffer it appends to, `beve_size_aligned_after` measures the same body given the same offset, and `beve::Writer::at` states the position outright, for a writer with no buffer to read it from or one whose buffer holds more than this document. A prefix of any length then works, rather than only one the widths happen to divide.

The reader still has to have its own buffer aligned before it can borrow anything out of it, which is the half the writer cannot settle.

Reading one costs no more than reading the plain form: this crate steps over the padding and takes the payload in the same single copy, so an aligned document is not a slower document here.

And it can cost nothing at all. `beve::Reader::try_slice::<f64>` hands back a `&[f64]` pointing into the document, and `Cow<'de, [f64]>` is the field type that reaches for one and copies when it cannot have it. Three things have to hold. The stored element type has to be exactly the one asked for, because widening is a conversion and a conversion is a copy. The host has to be little endian. And the payload has to begin on a multiple of the element width, which is what the form buys given a document that itself begins on one: a memory map is page aligned and an allocator hands back more alignment than `Vec<u8>` promises, but the language guarantees neither, so the borrow is offered rather than required. Nothing here fails because a document landed on an odd address; it copies instead.

Where the document is the array and nothing else -- a bulk numeric payload behind a protocol header, which is what the aligned form is usually written for -- `beve_slice_ref::<f64>(&bytes)` is that borrow without a cursor to place first. It is the stricter of the two: `try_slice` reads the array it is pointed at and says nothing about what follows, while the document form requires the array to be all of the document, so bytes behind it decline the borrow the way a mismatched element type does. For an array inside a larger document, `seek` to it and borrow at the cursor.

## Blocks, from a type this crate does not describe

A run of numbers is one header and one contiguous block, and that is most of why the format is worth having. Everything above gets it from the declaration. This is the same path reached by hand, for the case a declaration cannot cover: a scalar from a crate you do not own, which the orphan rule keeps `beve::Read` and `beve::Write` off, so the field names an [adapter](schemas.md#types-you-do-not-own) instead.

Two hooks per direction, and each pair exists on the type's trait and on the adapter's:

| | Writing | Reading |
|---|---|---|
| A type you own | `Write::ARRAY`, `Write::write_payload` | `Read::read_bulk` |
| Through an adapter | `WriteAs::ARRAY`, `WriteAs::write_payload` | `ReadAs::read_bulk` |

`ARRAY` is the header bytes a run is stored under, or `None` for a run that belongs in a generic array. `write_payload` fills what that header opened; the count and, in the aligned form, the padding are already out, so it appends elements and nothing else. `read_bulk` is offered the other end: the header and count consumed, the element header and the count handed over, and the cursor on the payload. It answers `false` to decline, which sends the caller down the ordinary element-by-element path with the cursor put back where it was.

All four default to the generic form, so an adapter that says nothing writes a generic array and reads it one value at a time. That is the right answer whenever the adapter has a conversion to do -- milliseconds to a `Duration`, a string to an address -- because there is no block to copy in the first place. It is the wrong answer only where the adapted type's memory *is* the payload, which is the case these are for.

What declining costs is the array's own header, read once per field and then put back: the same probe an unadapted `Vec` has always made, to the point that the compiler emits one body for both. It is not per element, and there is none of it on the writing side, where `ARRAY` is a constant and the match on it folds at monomorphization.

Saying so is `unsafe`, and it is one impl:

```rust
use structio::beve::NumericBytes;

#[derive(Clone, Copy)]
#[repr(transparent)]
struct Celsius(f64);

// SAFETY: `repr(transparent)` over `f64`, and the declared element is `f64`'s.
unsafe impl NumericBytes for Celsius {
    const ELEMENT: u8 = <f64 as NumericBytes>::ELEMENT;
}
```

`NumericBytes` is the bound on everything in this crate that reinterprets memory, and implementing it asserts four things: the type occupies `size_of::<Self>()` initialized bytes with no padding, every bit pattern of that size is a value, those bytes on a little-endian host are what BEVE stores for one `ELEMENT`, and one element is `size_of::<Self>()` bytes of payload. The fourth is the one a compiler can check and it is checked -- an impl whose declared element is not its own width is a build error naming the limit, reported when the crate is *built* rather than by `cargo check` or by an editor running one, the width being a constant of a generic type. The other three are why the trait is `unsafe`. `#[repr(transparent)]` over a primitive satisfies all four by construction and is the shape this expects.

With that, `Reader::read_block` and `Writer::write_block` are the copy in each direction, and they are what the two hooks call. Neither is `unsafe`, because neither can produce an invalid value -- every bit pattern of a `NumericBytes` type is one. What a wrong call produces is a wrong *answer*: taking a payload at some other width leaves the cursor inside the next value and the document reads on from there as if nothing had happened. So the element-type test at the top of `read_bulk` is not a formality, and neither is agreeing with the header `ARRAY` named.

Three things follow from all four hooks being reachable.

**The bytes are the same bytes.** An adapted `Vec` of a foreign scalar writes the typed array the bare `Vec` would have written, byte for byte, and reads it back in the same single copy. A pinned byte contract -- a golden file, a reader in another language, a hash over the encoding -- survives the move from a type to an adapter over it.

**`Same` is an identity on both halves.** `structio::Same` forwards `ARRAY` and `read_bulk` alike, so `xs as Vec<Same>` over a `Vec<f64>` is indistinguishable from `xs`, in the document and in the work done to produce it.

**The zero-copy borrow opens too.** `NumericBytes` is what `beve::Reader::try_slice` and `beve_slice_ref` are bounded on, so a foreign scalar that implements it can be borrowed straight out of an [aligned document](#arrays-a-reader-can-point-at) rather than copied at all.

## What is not implemented

- **128-bit floats are skipped, not decoded.** They are a valid width the specification defines and Rust has no type for.

## API

| Function | Purpose |
|---|---|
| `from_beve::<T>(&[u8]) -> Result<T>` | Parse into a new value. |
| `read_beve_into(&mut T, &[u8]) -> Result<()>` | Parse into an existing value, keeping its allocations. |
| `from_beve_at::<T>(&[u8], &str) -> Result<T>` | Parse the one value a JSON Pointer names. |
| `read_beve_into_at(&mut T, &[u8], &str) -> Result<()>` | The same, into an existing value. |
| `validate_beve(&[u8]) -> Result<()>` | Check a document is well formed, without decoding it. |
| `validate_beve_reader(impl io::Read) -> StreamResult<()>` | The same, over a reader. |
| `to_beve(&T) -> Vec<u8>` | Serialize. |
| `to_beve_aligned(&T) -> Vec<u8>` | Serialize with numeric typed arrays in the zero-copy aligned form. |
| `write_beve_into(&T, &mut Vec<u8>)` | Serialize into an existing buffer, keeping its allocation. |
| `append_beve(&T, &mut Vec<u8>)` | Serialize after what a buffer already holds. |
| `append_beve_aligned(&T, &mut Vec<u8>)` | The same in the aligned form, padded against the buffer's length. |
| `to_beve_writer(&T, impl io::Write) -> io::Result<()>` | Serialize into a sink, draining as it goes. |
| `beve_size(&T) -> usize` | The length `to_beve` would produce, without producing it. |
| `beve_size_aligned_after(&T, usize) -> usize` | The length `append_beve_aligned` would add behind a prefix of that length. |
| `from_beve_reader::<T>(impl io::Read) -> StreamResult<T>` | Read a whole document from a reader, then parse. |
| `read_beve_array_into(&mut Vec<T>, impl io::Read)` | Read a document that is one numeric or complex array, holding only the vector. |
| `from_beve_reader_array::<T>(impl io::Read)` | The same into a fresh vector. |
| `beve_slice_ref::<T>(&[u8]) -> Option<&[T]>` | Borrow a document that is one numeric array, or decline. |
| `beve::Reader::try_slice::<T>()` | The same at the cursor, for a block inside a larger document. |
| `beve::Reader::read_block(&mut Vec<T>, n)` | Take `n` elements of payload in one copy, for a `read_bulk` impl. |
| `beve::Writer::write_block(&[T])` | Append a block of payload in one copy, for a `write_payload` impl. |
| `beve::Documents::array(impl io::Read)` | Stream the elements of one top-level array. |
| `beve::Documents::values(impl io::Read)` | Stream whole documents written back to back. |
| `beve::Feed::array()` / `beve::Feed::values()` | The same two framings, for bytes pushed at you. |
| `beve_to_json(&[u8]) -> Result<String>` | Rewrite a document as JSON, with no type involved. |
| `beve_to_json_into(&[u8], &mut String) -> Result<()>` | The same, into an existing `String`. |
| `beve_to_json_writer(&[u8], impl io::Write) -> StreamResult<()>` | The same, into a sink, draining as it goes. |
| `Complex::new(re, im)` | A complex number, stored as the complex extension. |
| `Matrix::new(layout, extents, data) -> Result<Matrix<T>, ErrorCode>` | A matrix, stored as the matrix extension. |
| `MatrixRef::new(layout, &[usize], &[T]) -> Result<MatrixRef<T>, ErrorCode>` | The same, borrowed, for writing. |

Inside the `beve` module the format is dropped from the name, so `structio::to_beve` and `structio::beve::to_vec` are the same function.

For how the reader and writer work internally, including the implied-header mechanism and how typed arrays are dispatched without specialization, see [design.md](design.md#beve).
