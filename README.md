# structio

[![CI](https://github.com/stephenberry/structio/actions/workflows/ci.yml/badge.svg)](https://github.com/stephenberry/structio/actions/workflows/ci.yml)

**High-performance JSON and [BEVE](https://github.com/stephenberry/beve) for Rust structs.** No dependencies, no proc-macros, no intermediate representation.

Values are read straight into your types and written straight out of them. There is no `Value` enum, no token stream, and no document model: a field's bytes are converted exactly once, into the member that will hold them.

Built in the spirit of [Glaze](https://github.com/stephenberry/glaze).

---

## Installation

Not yet published to crates.io. Add it by git:

```toml
[dependencies]
structio = { git = "https://github.com/stephenberry/structio" }
```

Rust 2024 edition, MSRV 1.96.

## Quickstart

```rust
use structio::{from_beve, from_str, to_beve, to_string};

#[derive(Default, Debug, PartialEq)]
struct Config {
    name: String,
    port: u16,
    hosts: Vec<String>,
}

structio::object!(Config { name, port, hosts });

fn main() -> Result<(), structio::Error> {
    let text = r#"{"name":"api","port":8080,"hosts":["a","b"]}"#;

    // JSON, straight into the struct.
    let config: Config = from_str(text)?;
    assert_eq!(config.port, 8080);
    assert_eq!(to_string(&config), text);

    // The same schema, as BEVE binary. No second declaration.
    let bytes = to_beve(&config);
    let same: Config = from_beve(&bytes)?;
    assert_eq!(same, config);

    Ok(())
}
```

That is the whole API surface most work needs: declare the schema once, then convert. This example is [`examples/quickstart.rs`](examples/quickstart.rs), and a test fails if the two stop matching.

## Is this the right library for you?

**`serde` and `serde_json` are the right default for most Rust projects**, and this does not try to replace them. They have a vast ecosystem, support dozens of formats, and offer field attributes this crate has no answer to.

Reach for structio when one of these matters more:

| | |
|---|---|
| **You want BEVE** | A binary format that stays self-describing, so numeric arrays are a `memcpy` and documents are much smaller, without giving up the ability to skip a field you do not understand. |
| **You care about build time** | There is no proc-macro crate to compile and link before your own code can start building, and no dependency tree at all. |
| **You want to see what runs** | `object!`, `array!` and `tagged_enum!` expand to ordinary trait impls you could have written. There is no derive to reverse-engineer when something is slow or wrong. |
| **You are converting a fixed set of known types** | Which is what the design is optimized for, at the cost of not handling arbitrary documents at all. |

**Do not reach for it if** you need to inspect documents whose shape you do not know at compile time, you need a format other than JSON or BEVE, or you rely on `serde`'s attributes (`flatten`, `skip_serializing_if`, tagged enums, and so on). See [what it does not do](#what-it-does-not-do).

## Highlights

- **No dependencies.** Standard library only.
- **No proc-macros.** `object!`, `array!` and `tagged_enum!` are `macro_rules!` macros.
- **One schema, both formats.** The field list and its hash table are declared once and shared; only the bytes differ.
- **Objects or arrays.** `object!` declares a struct by key, `array!` by position, for types like a coordinate whose field names carry nothing.
- **Required members, one at a time.** A field marked `#[required]` has to be in the document; the rest keep their defaults when it is quiet. Mixed schemas are most schemas, so this is the type's business rather than a reader policy.
- **Enums by name, not by index.** `unit_enum!` writes a variant as its name; `tagged_enum!` writes one carrying a value as a one-member object keyed by that name. Adding or reordering variants does not change what a document already means.
- **Keys are hashed at compile time.** The macro picks the cheapest perfect hash that fits your key set, from a single byte comparison up to a full key hash.
- **Reads reuse what you already own.** Parsing into an existing value refills its buffers instead of reallocating them, so a loop over records of the same shape settles into no allocation at all.
- **One field out of a BEVE document.** `from_beve_at(&bytes, "/servers/1/port")` walks the headers in front of the value and decodes nothing else, and `validate_beve` checks a document is well formed without decoding any of it.
- **Both formats stream, both ways.** `Documents` and `Feed` hand out one value at a time from a reader or from chunks pushed at you, in JSON and in BEVE, so a file too large to hold costs one record rather than the file. A BEVE typed array streams element by element too.
- **Arrays a reader can point at.** `to_beve_aligned` writes BEVE's aligned typed arrays, padding each numeric payload onto its own element width so the block can be borrowed rather than copied. A `Cow<'de, [f64]>` field takes that borrow where the document allows it and copies where it does not. The same document either way, and every reader here takes both forms.
- **A BEVE document you have no type for.** `beve_to_json(&bytes)` rewrites it as JSON in one walk, with no tree and no schema, which is the answer to "what is actually in this file".
- **Complex numbers and matrices.** `Complex<T>` and `Matrix<T>` cover BEVE's two data-carrying extensions, in both formats. A `Vec<Complex<f64>>` is one header and one block, so it moves in a single copy exactly as a `Vec<f64>` does.
- **Errors carry a byte offset**, and `Error::display_with(input)` renders it with a line, column, and caret. A missing key also names itself, the offset there being able to point only at the object that lacks it.

## API

The crate root carries the JSON entry points unqualified and the BEVE ones with the format in the name. Inside the modules the format is dropped, so `structio::to_beve` and `structio::beve::to_vec` are the same function.

### JSON

| Function | Purpose |
|---|---|
| `from_str::<T>(&str) -> Result<T>` | Parse into a new value. |
| `from_str_with::<O, T>(&str) -> Result<T>` | Parse under a [read policy](docs/options.md): stepping over unknown keys, say. |
| `from_slice::<T>(&[u8]) -> Result<T>` | Parse bytes, validating UTF-8 once up front. |
| `read_into(&mut T, &str) -> Result<()>` | Parse into an existing value, keeping its allocations. |
| `to_string(&T) -> String` | Serialize. |
| `to_string_with::<O, T>(&T) -> String` | Serialize under a [write policy](docs/options.md): indented, or with nulls left out. |
| `to_vec(&T) -> Vec<u8>` | Serialize to bytes. |
| `write_into(&T, &mut String)` | Serialize into an existing buffer, keeping its allocation. |
| `to_writer(&T, impl io::Write)` | Serialize into a sink, draining as it goes. |
| `from_reader::<T>(impl io::Read)` | Read a whole document from a reader, then parse. |
| `prettify(&str) -> Result<String>` | Lay out JSON text that did not come from a `Write` impl. |
| `minify(&str) -> Result<String>` | Take the whitespace back out of JSON text. |
| `Documents<R>` | Pull a sequence of values out of a reader. |
| `Feed` | Push chunks in, take values out as they complete. |

### BEVE

| Function | Purpose |
|---|---|
| `from_beve::<T>(&[u8]) -> Result<T>` | Parse into a new value. |
| `from_beve_with::<O, T>(&[u8]) -> Result<T>` | Parse under a [read policy](docs/options.md). |
| `read_beve_into(&mut T, &[u8]) -> Result<()>` | Parse into an existing value, keeping its allocations. |
| `from_beve_at::<T>(&[u8], &str) -> Result<T>` | Parse the one value a JSON Pointer names, skipping the rest. |
| `read_beve_into_at(&mut T, &[u8], &str) -> Result<()>` | The same, into an existing value. |
| `validate_beve(&[u8]) -> Result<()>` | Check a document is well formed, without decoding it. |
| `validate_beve_reader(impl io::Read) -> StreamResult<()>` | The same, over a reader. |
| `to_beve(&T) -> Vec<u8>` | Serialize. |
| `write_beve_into(&T, &mut Vec<u8>)` | Serialize into an existing buffer, keeping its allocation. |
| `to_beve_writer(&T, impl io::Write)` | Serialize into a sink, draining as it goes. |
| `append_beve_aligned(&T, &mut Vec<u8>)` | Serialize after what a buffer holds, padded against its length. |
| `beve_size(&T) -> usize` | The length `to_beve` would produce, without producing it. |
| `beve_size_aligned_after(&T, usize) -> usize` | The same for an aligned body landing behind a prefix. |
| `from_beve_reader::<T>(impl io::Read)` | Read a whole document from a reader, then parse. |
| `read_beve_array_into(&mut Vec<T>, impl io::Read)` | Read one enormous numeric array from a reader, without holding its encoded form. |
| `beve_slice_ref::<T>(&[u8]) -> Option<&[T]>` | Borrow an aligned numeric array out of a document, copying nothing. |
| `Complex::new(re, im)` | A complex number, stored as BEVE's complex extension. |
| `Matrix::new(layout, extents, data)` | A matrix, stored as BEVE's matrix extension. |
| `MatrixRef::new(layout, &[usize], &[T])` | The same, borrowed, for writing data you already hold. |

`read_into` and `write_into` are the ones to reach for in a loop. `read_into` is also the way in for a type with no meaningful zero value: only the functions that *return* a `T` need `Default`, and a placeholder to read over [does not have to be public](docs/schemas.md#default-is-required-where-values-are-constructed).

### Between the formats

| Function | Purpose |
|---|---|
| `beve_to_json(&[u8]) -> Result<String>` | Rewrite a BEVE document as JSON, with no type involved. |
| `beve_to_json_into(&[u8], &mut String)` | The same, into an existing `String`. |
| `beve_to_json_writer(&[u8], impl io::Write)` | The same, into a sink, draining as it goes. |

## Documentation

| | |
|---|---|
| [Schemas and types](docs/schemas.md) | Renaming keys, required fields, positional structs, generics and borrowing, the supported type set, writing impls by hand. |
| [Enums](docs/enums.md) | The two wire forms and which of them reading accepts, renaming variants, what is refused and with which error, the policies, and the BEVE string-array form. |
| [BEVE](docs/beve.md) | What the binary format buys you, pointers and validation, turning a document you have no type for into JSON, and what is not implemented yet. |
| [Options](docs/options.md) | Indenting JSON, keeping arrays on one line, leaving null members out, refusing unknown keys, requiring declared ones, reading comments, prettifying and minifying text that is already JSON, and writing your own policy. |
| [Streaming](docs/streaming.md) | Documents too large to hold, or not yet fully arrived, in either format. |
| [Errors](docs/errors.md) | What each code means and how to render one. |
| [Performance](docs/performance.md) | Benchmarks against Glaze, methodology, and how to reproduce them. |
| [Correctness](docs/correctness.md) | What is tested, and how. |
| [Design notes](docs/design.md) | How it works inside. |

`cargo doc --open` builds the API reference.

## Performance in one paragraph

Against Glaze on identical documents, reading is 50-112% and writing 66-155% depending on the type. Writing integers and booleans is faster than Glaze; strings and floats are below it. Output is byte-identical to Glaze's on every benchmark document, floats included. Full table, methodology, and an important caveat about how sensitive the benchmark is to code layout: [docs/performance.md](docs/performance.md).

No comparison against `serde_json` has been run, so please do not infer one.

## What it does not do

**Only statically known types.** There is no lazy or generic value type. If you need to reach into arbitrary documents from code, this is the wrong tool. Reading one is a different matter: `beve_to_json` turns any BEVE document into JSON without a schema.

**No `json_to_beve`.** BEVE prefixes every container with its count and JSON gives that up only at the end, so the reverse of the above is not the same one-pass walk. JSON with a schema is `from_str::<T>` and then `to_beve`.

**Two formats.** Of the formats Glaze supports, JSON and BEVE are here; CSV, TOML, and the rest are not.

**Few options.** [`Options`](docs/options.md) covers indentation, keeping an array on the line it began on, leaving null members out, refusing a key nothing claims, requiring every key the schema declares, and reading JSONC comments, which is where Glaze's `glz::opts` starts. Requiring *some* of the keys is a property of the schema rather than a policy: mark those fields [`#[required]`](docs/schemas.md#required-fields).

**Few `serde` attributes.** Keys can be renamed one at a time or by a [case rule](docs/schemas.md#case-rules), fields can be marked [`#[required]`](docs/schemas.md#required-fields), null members can be skipped, and that is the extent of it. There is no `flatten` and no enum tagging strategy.

**Foreign types need an adapter or a wrapper.** Rust's orphan rule means you cannot describe a type from another crate the way you can specialize `glz::meta` for any C++ type. A field can name an [adapter](docs/schemas.md#types-you-do-not-own) that says how its type is read and written, which keeps the type out of your API; a newtype is still the answer when the foreign type has no `Default`, or when it appears in many structs.

## Status

Version 0.1.0, unpublished. The API is not yet frozen and the version number should be taken at face value. What changes between releases is in [CHANGELOG.md](CHANGELOG.md).

What that does *not* mean is untested. See [docs/correctness.md](docs/correctness.md) for the fuzzing, the exhaustive `f32` sweep, the Miri coverage of every `unsafe` block, and the byte-for-byte cross-check of the BEVE encoding against an independent implementation.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

### Third-party code

`src/num/zmij.rs` is a port of [zmij](https://github.com/vitaut/zmij) by Victor Zverovich, used for shortest round-trip float formatting. The algorithm, its lookup-table seeds, and its SWAR digit split are his work, and it carries his copyright under the terms in [LICENSE-THIRD-PARTY](LICENSE-THIRD-PARTY). Nothing else in the crate is derived from another project.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
