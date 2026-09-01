# Performance

## What is measured

The numbers below are **JSON**, against [Glaze](https://github.com/stephenberry/glaze), on identical documents, the same machine, and the same data.

Glaze is the comparison because it is the fastest JSON library the author knows of and because structio is deliberately built in its spirit, so the gap is the interesting number. There is **no comparison against `serde_json` here**, because none has been run. Do not infer one.

BEVE has no comparable published baseline to sit beside, and its cost is dominated by memory bandwidth rather than by parsing: a numeric array moves at `memcpy` speed in both directions, and a scalar is a header byte and a load.

## Results

Apple M-series, Rust 1.98 `-C lto=fat -C codegen-units=1`, Clang `-O3 -march=native`, throughput in MB/s of the document, best of two runs on an otherwise idle machine.

| document | bytes | read | Glaze | | write | Glaze | |
|---|---:|---:|---:|---:|---:|---:|---:|
| mixed | 248606 | 906 | 1288 | 70% | 1567 | 1816 | 86% |
| strings | 127921 | 1762 | 1576 | **112%** | 1974 | 2799 | 71% |
| uints | 30085 | 803 | 1603 | 50% | 1737 | 1345 | **129%** |
| ints | 31679 | 724 | 1356 | 53% | 1822 | 1177 | **155%** |
| doubles | 67154 | 656 | 1155 | 57% | 1398 | 1966 | 71% |
| exact decimals | 29251 | 553 | 627 | 88% | 580 | 880 | 66% |
| bools | 25675 | 1439 | 1690 | 85% | 4870 | 3831 | **127%** |

`mixed` is the representative case: the standard Glaze/JSONifier benchmark structure, 26 fields of nested structs each holding vectors of strings, unsigned ints, doubles, signed ints, and bools. The single-type documents isolate one converter each and are dominated by that converter, which is what makes them useful for finding weaknesses and misleading as a summary.

## Where it stands

Reading is 50-112% of Glaze. Writing integers and bools is well above Glaze, strings and floats below.

Float writing used to be the one real weakness, at 26-39%. `src/num/zmij.rs` replaced Ryu with a port of [zmij](https://github.com/vitaut/zmij), the algorithm Glaze itself uses, which took it to 66-71%:

| | before | after |
|---|---:|---:|
| doubles, write | 726 MB/s | 1398 MB/s |
| exact decimals, write | 218 MB/s | 580 MB/s |
| mixed, write | 1187 MB/s | 1567 MB/s |

Measured per value, the whole write went from 25.7 ns for an arbitrary double and 28.0 ns for an exact short decimal, to about 14 ns for both. The two numbers converging is the point: Ryu removes digits one at a time, so it is slowest on the values real documents are full of, while zmij does the same work for every input.

One difference remains between this port and Glaze's copy: Glaze splits digits with SSE/NEON intrinsics and assembles the output through a table of precomputed field positions, where this takes zmij's portable scalar path for both, because the crate carries no architecture-specific code. Whether that accounts for the remaining gap has not been measured.

Output is byte-identical to Glaze's on the benchmark documents, including float formatting, which the benchmark itself checks.

## Prettifying

`prettify` lays out JSON text that arrived as text, with no type in the way, against `glz::prettify_json`. Both sides read the same compact document and indent it three spaces to a level, which is Glaze's default width; throughput is MB/s of the *input*.

| document | bytes | structio | Glaze | |
|---|---:|---:|---:|---:|
| mixed | 248606 | 672 | 748 | 90% |
| strings | 127921 | 1192 | 1272 | 94% |
| uints | 30085 | 441 | 442 | 100% |
| ints | 31679 | 387 | 400 | 97% |
| doubles | 67154 | 663 | 763 | 87% |
| exact decimals | 29251 | 313 | 364 | 86% |
| bools | 25675 | 476 | 526 | 90% |

86-100%, level with Glaze on unsigned integers. The two do not check quite the same things: Glaze's prettifier is a flat token scanner that does not verify a literal spells `true`, that brackets match, or that an object alternates keys and colons, where this one walks the document with the parser and reports a structural failure at the byte that stopped it. Neither holds a number to the JSON grammar. The output is byte-identical to Glaze's, which the benchmark checks by reading back the file the C++ side leaves beside each document.

The costs that were worth removing, in the order they mattered: skipping whitespace once per token boundary rather than once per value as well (bools 327 to 378 MB/s, strings 1019 to 1218); emitting `true`, `false` and `null` as constant runs through `append_fixed` rather than copying them out of the input at a runtime length (bools 378 to 452); and stepping over a number by its alphabet rather than holding it to the JSON grammar, which is worth a further 0-5%, worst on integer-heavy documents. The last one is a trade rather than a free win, and [design.md](design.md) has the argument.

## Minifying

`minify` takes the whitespace back out, against `glz::minify_json`. Both sides read the laid-out document from the section above, so throughput is MB/s of the *input*, which is the larger file.

| document | bytes | structio | Glaze | |
|---|---:|---:|---:|---:|
| mixed | 466097 | 2279 | 1983 | 115% |
| strings | 177348 | 2002 | 2410 | 83% |
| uints | 79512 | 1881 | 1736 | 108% |
| ints | 81106 | 1585 | 1420 | 112% |
| doubles | 116581 | 2552 | 1722 | 148% |
| exact decimals | 78678 | 1636 | 1381 | 118% |
| bools | 75102 | 1745 | 2670 | 65% |

Ahead on five of the seven and behind on the two whose documents are the most whitespace by proportion. The output is checked, not just timed: minifying Glaze's own laid-out form has to reproduce the compact document Glaze wrote in the first place, byte for byte.

Three things carried it.

**Copy in blocks of a compile-time-constant size.** A minifier copies a document in small pieces, a key or a number or a `true` at a time, and a `memcpy` whose length the compiler cannot see costs more than the bytes. Copying a fixed 16 or 64 and keeping only the run's length is the same trick the writer already uses for `true` and small integers. Both rungs earn their place and a third between them does not: 16 alone leaves `doubles` about a third short, since a double is 17 to 20 characters, while adding 32 to the pair moves nothing on any document.

**Compare each run of whitespace against the run before it.** Indentation repeats, so the previous run is nearly always the same bytes, and one eight-byte compare per eight bytes of it beats walking them; this is Glaze's `skip_matching_ws`. Measured on its own against a stubbed-out version it is worth 30-38% at the benchmark's three-space width and 7-20% at this crate's own two-space default. The gap between those two figures is the mechanism's one limitation: it gives up on runs shorter than eight bytes, so at two spaces it does nothing until a document is four deep. Runs that short are seven byte-comparisons at worst, which is not enough work to be worth a masked compare and its own bounds case.

**Scan a string for the closing quote and nothing else.** The reader's scan looks for a backslash and a control character alongside the quote, because it has to unescape one and refuse the other. Copying a string through needs neither: an escape goes out as it came in, and the only question is where the string stops. Asking one question instead of three was worth 3% on the string-heavy document when it went in, before the copy was fast enough for the scan to be most of the work.

What is left is where Glaze wins: its token dispatch knows a `true` from its first byte and steps over four bytes without looking at them, while this scans for where the token ends. That shows up exactly where tokens are shortest and whitespace is two thirds of the file.

## A caveat about reading these numbers

This benchmark is sensitive to code layout to a degree that exceeds several of the differences in the table. Adding an unused function, or a struct field that is never read, has been observed to move a single document's result by 6-15% while leaving the others inside noise.

So a change of a few percent on one row, with the others unmoved, is not evidence of anything. Treat the table as showing where the library stands in broad terms, not as a instrument fine enough to attribute small deltas to specific commits. When a change should be strictly less work, prefer proving it changed no *answer* over proving it changed the clock.

## Reproducing

```sh
c++ -std=c++23 -O3 -DNDEBUG -march=native \
    -I /path/to/glaze/include \
    benches/baseline/glaze_baseline.cpp -o tmp/glaze_baseline
./tmp/glaze_baseline       # generates tmp/*.json and prints Glaze's numbers
cargo bench --bench roundtrip
```

The C++ baseline generates the documents and reports Glaze's numbers; `benches/roundtrip.rs` reads the same files, so both sides are measured on identical bytes. See [`benches/baseline/README.md`](../benches/baseline/README.md).

## Where the speed comes from

Nothing here is a micro-optimization applied after the fact; the shape of the library is the optimization.

- **No intermediate representation.** A field's bytes are converted exactly once, into the member that will hold them. There is no `Value` enum, no token stream, and no document model to build and then walk.
- **Keys are hashed at compile time.** The macro picks the cheapest perfect hash that fits your key set, from a single byte comparison up to a full key hash. Both formats look keys up in that one table.
- **Reads reuse what you already own.** Parsing into an existing value refills its buffers instead of reallocating them, which is what makes a loop over records of the same shape settle into doing no allocation at all.
- **BEVE numeric arrays are bulk copies** in both directions when the stored element type matches the destination's, and either form is taken whole: the aligned one is not a slower document to read here. It can also be no copy at all. `to_beve_aligned` pads each payload onto its own element width, and a `Cow<'de, [f64]>` field then points into the document instead of copying out of it, when the document's own address allows it.

[design.md](design.md) has the detail, including the two inlining decisions that mattered most and the places where Rust forced a different answer than the C++ original.
