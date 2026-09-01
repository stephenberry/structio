# structio documentation

Start with the [README](../README.md) for installation and a first example. These pages go deeper.

## Using the library

| | |
|---|---|
| [Schemas and types](schemas.md) | Declaring a struct's schema as an object or as an array, declaring an enum, renaming keys, generics and borrowing, the supported type set, and writing the impls by hand. |
| [BEVE](beve.md) | What the binary format buys you, how it differs from JSON in practice, reaching one field without decoding the rest, turning a document you have no type for into JSON, complex numbers and matrices, writing arrays a reader can point at and borrowing one back, framing a body whose length has to be sent before it or that lands behind a header, and what is not implemented yet. |
| [Options](options.md) | Indenting JSON, keeping arrays on one line, leaving null members out, refusing unknown keys, requiring declared ones, reading comments, laying out and minifying text that is already JSON, writing your own policy, and what the compile-time seam costs. |
| [Streaming](streaming.md) | Reading documents that do not fit in memory, or have not fully arrived, and writing without assembling the output first. Both formats. |
| [Errors](errors.md) | What errors carry, how to render them, and what each code means. |

## Understanding the library

| | |
|---|---|
| [Performance](performance.md) | Benchmark results against Glaze, the methodology, and how to reproduce them. |
| [Correctness](correctness.md) | What is tested and how, including the fuzzing, the exhaustive float sweep, and the Miri coverage. |
| [Design notes](design.md) | How compile-time key hashing works, what the parser and writer do differently from the obvious implementation, and the trade-offs behind both. |
| [Schema declaration](schema-declaration.md) | Why `object!` is a `macro_rules!` macro rather than a `#[derive]`, compared against the alternatives that were considered. |

## Reference

`cargo doc --open` builds the API reference. Every public item carries documentation, and the crate builds clean under `cargo doc`.
