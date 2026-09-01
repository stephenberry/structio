# Streaming

Documents that do not fit in memory, or have not fully arrived, go through the streaming API. It is the same parser and the same writer underneath, so streamed and slurped documents cannot disagree about what a document means.

Both formats stream, in both directions. The JSON entry points are unqualified at the crate root; the BEVE ones live in `structio::beve`. This page describes the JSON side first because the two work identically; [BEVE](#beve) covers what differs.

## Writing

`to_writer` drains into an `io::Write` as the document is produced, so peak memory does not follow the size of the output:

```rust
structio::to_writer(&records, std::fs::File::create("out.json")?)?;
```

`to_writer_buffered` takes an explicit buffer size for when the default does not suit.

`beve_to_json_writer` drains a transcode the same way, so the JSON a BEVE document turns into never has to exist all at once. The BEVE itself still does, since a value is located by stepping over the ones in front of it.

The exact bound differs by format. `to_beve_writer` holds to its buffer and nothing more: a value too large to fit is one contiguous block, and it is handed to the sink directly rather than copied in first. JSON's buffer can still grow to hold one long run of string text, because the writer keeps its tail available to rewrite a trailing comma into a closing brace, so a run cannot be passed straight through.

There is no separate "streaming writer" type to learn. The ordinary `Write` impls do not know whether their output is going to a `String` or to a socket. That holds for the [aligned form](beve.md#arrays-a-reader-can-point-at) too: `Writer::to_sink(&mut out).aligned()` pads against where each array lands in the document rather than in the buffer, so a drained write is byte for byte the one an in-memory writer would have produced.

## Reading a sequence of values

`Documents` turns a reader into a series of values, holding one at a time. It covers the three shapes a JSON stream comes in:

| Constructor | Input shape |
|---|---|
| `Documents::lines(r)` | Newline-delimited records (NDJSON). CRLF and a missing final newline are both accepted. |
| `Documents::array(r)` | The elements of one large top-level array. |
| `Documents::values(r)` | Bare values back to back, with or without whitespace between them. |

```rust
let file = std::io::BufReader::new(std::fs::File::open("records.ndjson")?);
let mut docs = structio::Documents::lines(file);
for record in docs.iter::<Record>() {
    println!("{}", record?.id);
}
```

The splitter divides the stream itself, so it takes the read policy along with the parser: `with_options::<AllowComments>()` teaches both, because a comment may hold a brace and a splitter that did not know about one would cut the stream in the wrong place. The scan resumes inside a comment as it does inside a string, so a refill landing between the two bytes that close one costs nothing. `Mode::Lines` is the exception, and only for block comments; see [options](options.md#allow_comments).

Three ways to take values out:

- `iter::<T>()` is the ergonomic form, yielding `StreamResult<T>`.
- `next_value::<T>()` is the borrowing form. The value may point into the stream buffer, so the borrow pins the reader until it is dropped, which is what lets `&'de str` fields work.
- `next_value_into(&mut value)` reads into a value you already have. A loop over a million records of the same shape settles into doing no allocation at all.

`with_options::<O>()` sets the [read policy](options.md) for every value the stream produces, alongside `max_value` and `read_size` in the same builder chain. Reading a subset of each record wants `SkipUnknown`, since the default refuses the first key the destination does not claim:

```rust
let mut docs = structio::Documents::lines(file).with_options::<structio::SkipUnknown>();
```

`Feed` takes the same method.

## Being handed bytes

`Feed` is the same machine driven from the other side, for when something gives you chunks rather than letting you ask for them:

```rust
let mut feed = structio::Feed::values();
feed.push(&chunk_from_socket);
while let Some(value) = feed.next_value::<Record>() {
    handle(value?);
}
```

It has the same three framings and the same `next_value` / `next_value_into` pair.

Chunks may split a value at **any** byte, including inside a string, inside an escape, or between the digits of a number. The scan that finds where a value ends carries its state across the boundary.

## BEVE

`beve::Documents` and `beve::Feed` are the same machines over binary input, with two framings instead of three:

| Constructor | Input shape |
|---|---|
| `beve::Documents::values(r)` | Whole documents back to back. Nothing separates them, since every value states its own extent; the specification's delimiter extension may appear between them and is stepped over. |
| `beve::Documents::array(r)` | The elements of one large top-level array. |

```rust
let file = std::io::BufReader::new(std::fs::File::open("samples.beve")?);
let mut docs = structio::beve::Documents::array(file);
for sample in docs.iter::<Sample>() {
    handle(sample?);
}
```

`array` accepts a typed array as well as a generic one. A typed array stores one header for a whole run of elements, so an element cut out of it is not by itself a value anything could read; the splitter hands the header the array implied to the reader alongside the span. A file that is one enormous `Vec<f64>` therefore streams as `f64`s rather than being buffered whole. What that gives up is the bulk path: read whole, a million samples are one `memcpy`; streamed, they are a million reads of eight bytes. Stream the array when it does not fit, read it whole when it does.

`beve::from_reader` is not this. It is `read_to_end` and then `from_beve`, so it holds the whole document; the same is true of `validate_beve_reader`.

### One array, without the element-by-element price

A document that *is* one enormous numeric array is the case `Documents::array` serves worst: one whole value is its floor, so it has to hand out `f64`s, and that is exactly where the block copy was worth having. `read_beve_array_into` reads that shape directly.

```rust
let file = std::io::BufReader::new(std::fs::File::open("samples.beve")?);
let mut samples: Vec<f64> = Vec::new();
structio::read_beve_array_into(&mut samples, file)?;
```

The payload goes from the reader into the vector's own memory, so the vector is the only thing held rather than the vector plus the encoded document, and the copy stays one block per megabyte instead of one per element. Reading into a vector that already has the capacity holds only that, growing one being a `Vec` growing like any other. Both array forms are accepted, plain and aligned. Reading into a vector you keep is what makes a loop cost one array rather than two; `from_beve_reader_array` is the same read into a fresh one.

It is exact where the rest of the crate is lenient. The stored element type has to be `T`'s, and a stored `f32` read as `f64` is `ElementTypeMismatch` rather than the widening `from_beve` performs happily. Converting is per-element work, which is the thing this call exists to skip, so it says so instead of quietly becoming `Documents::array`. Big-endian hosts are not excluded: elements are swapped in place after the copy, which a borrow cannot do and this can.

There is no `max_value` here and none is needed. A count read off the wire is never reserved on its word, so a length that is never delivered costs about what did arrive and then fails with `UnexpectedEnd`.

The array has to be the whole document, so a stream carrying anything else has to be bounded to it first. Where the length is known, as it is in a framed protocol, `Read::take` is the whole of it: the frame's end is the document's end, and the reader is left standing on the next frame. Where the array follows other values on the same stream, read those with `Documents::values` and then `into_parts`, which hands back the reader together with the bytes read past them; chaining the two is a stream that begins at the array.

```rust
let mut docs = structio::beve::Documents::values(stream);
let header: Header = docs.next_value().unwrap()?;
let (rest, unread) = docs.into_parts();

let mut samples: Vec<f64> = Vec::new();
structio::read_beve_array_into(&mut samples, (&unread[..]).chain(rest))?;
```

That is the layout to reach for when a payload is a header and a bulk block. Written as two values it streams; written as one struct holding the block as a field it does not, since reaching a field means reading the struct and reading the struct means holding the document.

### What differs from the JSON side

Framing is a walk rather than a scan. JSON hides the byte that ends a value behind nesting and quoting, so its splitter looks at every byte of the input. BEVE states every extent up front, so nothing in the BEVE splitter looks at a payload at all: it reads headers, counts and object keys, and everything else is a number of bytes to step over.

That makes suspending harder rather than easier, and it changes what an untrusted producer can do to you. A hostile JSON document is one that never closes a bracket; a hostile BEVE document is one that claims a length it never delivers. `max_value` is the answer to both.

There is no `Mode::Lines` equivalent and there cannot be, since no byte is forbidden inside a BEVE payload. There is also no resynchronization: a corrupt record ends a BEVE stream where NDJSON would lose only the line.

A complex number and a matrix each stream as one whole value, in `values` mode like anything else, since the splitter frames both from their stated extents and looks at neither payload. `array` mode goes further and hands out the *elements* of a top-level complex array one at a time, exactly as it does a typed array's, so a run of complex samples larger than memory streams as `Complex` values. That needed nothing new: a complex element carries no header of its own, and the synthetic one the splitter reports is a single byte carrying the class and the width, which is precisely what the mechanism already took.

## What streaming does not do

It does not hand back a half-filled struct. A value is parsed once all of its bytes are present.

That is a deliberate trade. It is what keeps borrowed `&str` fields working, and what keeps the streaming and batch paths from ever diverging in what they accept. The cost is that **memory is bounded by the largest single value, not the largest document**. For NDJSON and array modes that bound is one record, which is the case those formats exist to serve. For a single enormous object it is the whole object, and streaming buys you nothing. A single enormous *numeric array* is the one shape with a way out, `read_beve_array_into` above, because a block of numbers is the one value that can be built as it arrives. A struct with one enormous array *field* is not that shape: the block is bulk-readable in principle, but reaching the field means reading the struct. Writing the two as separate values is what turns it back into the shape that streams.

## Errors and recovery

A record that will not parse is reported and skipped. The framing is still intact, so reading carries on with the next one, which is what makes per-record error recovery work on a large file.

A failure to *frame* is different. The position in the input is then unknown, so it is reported once and ends the stream.

Errors carry their offset against the whole stream, not against the current buffer. `StreamError` separates the two things that can go wrong:

```rust
match err {
    StreamError::Io(e) => /* the reader failed */,
    StreamError::Parse(e) => /* the bytes were not what was expected; e.offset locates it */,
    _ => /* the enum is `#[non_exhaustive]`, so this arm is required */,
}
```

`as_parse()` and `as_io()` are the accessor forms, and avoid the catch-all arm.

## Getting the reader back

`into_inner()` returns the reader and drops the window, which is lossy: reads happen a chunk at a time, so bytes past the last value returned have usually already been taken from the reader. This matches `io::BufReader::into_inner`.

`into_parts()` returns `(reader, Vec<u8>)` instead, where the bytes are everything read but not yet resolved into a value. Those bytes followed by whatever is still in the reader reconstruct the rest of the stream exactly, so this is the form to use when handing a partly consumed stream on to something else.

## One deliberate divergence from the batch API

An input holding nothing but whitespace is an **empty stream**: zero values, no error. `from_str` rejects the same input, because there a document was asked for and none was found. An empty BEVE input is the same, in either mode.

Everything else the streaming side accepts, the batch side accepts. That is checked as a property over generated inputs rather than asserted in prose, in both formats.

## Untrusted input

There is no size limit by default. When the producer is not trusted, set one:

```rust
let docs = structio::Documents::lines(reader).max_value(1 << 20);
```

Reads are clipped so the window never runs more than a byte past the limit, and exceeding it is `ErrorCode::DocumentTooLarge`.

For how framing works internally, and why draining costs the in-memory path nothing, see [design.md](design.md#streaming).
