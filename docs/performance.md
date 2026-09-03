# Performance

## What is measured

The numbers below are **JSON**, against [Glaze](https://github.com/stephenberry/glaze), on identical documents, the same machine, and the same data.

Glaze is the comparison because it is the fastest JSON library the author knows of and because structio is deliberately built in its spirit, so the gap is the interesting number. There is **no comparison against `serde_json` here**, because none has been run. Do not infer one.

BEVE has no comparable published baseline to sit beside, and its cost is dominated by memory bandwidth rather than by parsing: a numeric array moves at `memcpy` speed in both directions, and a scalar is a header byte and a load.

## Results

Apple M-series, Rust 1.98 `-C lto=fat -C codegen-units=1`, Clang `-O3 -march=native`, throughput in MB/s of the document, best of five runs on an otherwise idle machine, with the binary built before any run is timed so that no compile competes with one.

| document | bytes | read | Glaze | | write | Glaze | |
|---|---:|---:|---:|---:|---:|---:|---:|
| mixed | 248606 | 1000 | 1277 | 78% | 1526 | 1701 | 90% |
| strings | 127921 | 1649 | 1565 | **105%** | 2346 | 2574 | 91% |
| uints | 30085 | 1241 | 1547 | 80% | 1728 | 1323 | **131%** |
| ints | 31679 | 1133 | 1317 | 86% | 1780 | 1135 | **157%** |
| doubles | 67154 | 647 | 1122 | 58% | 1396 | 1954 | 71% |
| exact decimals | 29251 | 582 | 611 | 95% | 569 | 855 | 67% |
| bools | 25675 | 1402 | 1660 | 84% | 5615 | 3479 | **161%** |

`mixed` is the representative case: the standard Glaze/JSONifier benchmark structure, 26 fields of nested structs each holding vectors of strings, unsigned ints, doubles, signed ints, and bools. The single-type documents isolate one converter each and are dominated by that converter, which is what makes them useful for finding weaknesses and misleading as a summary.

## Where it stands

Reading is 58-105% of Glaze. Writing integers and bools is well above Glaze, strings and floats below.

Float writing used to be the one real weakness, at 26-39%. `src/num/zmij.rs` replaced Ryu with a port of [zmij](https://github.com/vitaut/zmij), the algorithm Glaze itself uses, which took it to 66-71%:

| | before | after |
|---|---:|---:|
| doubles, write | 726 MB/s | 1398 MB/s |
| exact decimals, write | 218 MB/s | 580 MB/s |
| mixed, write | 1187 MB/s | 1567 MB/s |

Measured per value, the whole write went from 25.7 ns for an arbitrary double and 28.0 ns for an exact short decimal, to about 14 ns for both. The two numbers converging is the point: Ryu removes digits one at a time, so it is slowest on the values real documents are full of, while zmij does the same work for every input.

One difference remains between this port and Glaze's copy: Glaze splits digits with SSE/NEON intrinsics and assembles the output through a table of precomputed field positions, where this takes zmij's portable scalar path for both, because the crate carries no architecture-specific code. Whether that accounts for the remaining gap has not been measured.

Output is byte-identical to Glaze's on the benchmark documents, including float formatting, which the benchmark itself checks.

### The integer read gap was a call, not the digits

The obvious reading of the `uints` row used to be that the conversion was slow, since `atoi.hpp` unrolls a per-digit chain where `src/num/atoi.rs` folds whole eight-digit words and then loops over the tail, and the benchmark's values are one to seven digits each, so the word path never fires. Two rewrites of the conversion were tried on that theory. Both were slower than the loop they replaced, measured on a 200,000-number run: a SWAR mask locating the terminator went from 5.16 to 5.74 ns per value, and Glaze's own shape, nineteen unrolled digit tests with the bounds settled once up front, went to 14.19. Neither moved the document benchmark at all.

What the disassembly showed instead is that the element loop was making a **call per element**. `<u64 as Read>::read` was left out of line, so every integer in the document cost an argument setup, a call, and the parser's cursor spilled to the stack and reloaded on return:

```
4e4:  add  x1, sp, #0x8      ; &mut Parser, from a stack slot
4e8:  bl   <u64 as Read>::read
4f4:  ldp  x1, x9, [sp, #0x10] ; reload data and idx afterwards
```

Glaze's array read has no call in it at all; its only ones are `operator new`, `operator delete` and unwinding. `#[inline]` is a hint, and LLVM was declining it because the transitive body, both digit loops plus the overflow recheck, priced it out.

The fix is to make the common case small enough to be worth inlining rather than to ask harder. `parse_u64` now handles one to eighteen digits ending inside the buffer, which is a loop of about a dozen instructions carrying no overflow check, and hands everything else to `parse_u64_wide` out of line. That is enough for the whole chain to inline on its own. `Parser::read_u64`, `read_i64` and the integer `Read::read` are `#[inline(always)]` rather than `#[inline]`, because leaving them as hints makes the array loop's throughput depend on whether anything downstream has recently grown.

The second difference the disassembly showed was whitespace. A four-way `matches!` does not stay four comparisons: it becomes a shift and a mask against a 64-bit set, plus a range guard, because a byte of 64 or more would shift out of the word. Glaze indexes a 256-byte table and needs no guard. Four instructions per token boundary became one.

| read, MB/s | before | inlined | + ws table | Glaze |
|---|---:|---:|---:|---:|
| uints | 897 | 1101 | **1241** | 1547 |
| ints | 793 | 933 | **1133** | 1317 |
| mixed | 900 | 964 | **1000** | 1277 |
| strings | 1528 | 1547 | **1649** | 1565 |
| exact decimals | 548 | 531 | **582** | 611 |

The lesson worth keeping is that the conversion was never the cost. Stubbing it out entirely, before any of this, took `uints` only from 900 to 1391, because the call stayed. Look at what the compiler emitted before rewriting what the source says.

### The float read was the same call, and the loops were the rest

The float row was the widest gap in the table above, at 58%, and the disassembly showed the same shape the integer path had before: `<f64 as Read>::read` left out of line and called once per element, with the parser's cursor spilled around it. It was out of line for a reason that was not the float code's size at all. `Vec<T>`'s reader had two call sites for the element read, one for a slot it already held and one for a fresh value it would push, and the compiler was asked to inline the whole conversion into both. Pushing a default first and then reading into the slot, the same path for both cases, halved what was being asked, and with `read_f64` marked the way `read_u64` already was, the conversion sat in the loop.

The second thing the disassembly showed was that the cursor was still in memory even where nothing was called per element. `parse_u64_wide`, the out-of-line path for wide integers, took the cursor as `&mut usize`, and a value whose address is handed to any call has to live on the stack for the whole of the loop around that call, stored before and reloaded after each element whether the call happens or not. Every out-of-line helper on these paths now takes the cursor by value and returns the new one, and the bool reader's cold fallback, which had been added taking `&mut self`, was measured putting the cursor back on the stack before it was changed to the same shape. The rule that fell out is worth stating on its own: **a `&mut` to the parser or its cursor, passed to anything that is not inlined, costs the loop it sits in a memory round trip per element.**

Third, the branches that depend on the data. The benchmark's signs are random, and so are its bools, and a branch on either mispredicts on about every other value at ten to fifteen cycles each, which is more than the conversion costs. The sign of an integer is now applied through a mask, the sign of a float by setting its sign bit, and `true` and `false` are told apart by one eight-byte load compared against whichever the first byte says to expect, chosen with a select rather than a branch. That last one is where the compiler has to be watched: written as `is_true | is_false` over two compares it was turned back into a branch on each, and the document benchmark said so before the assembly did.

Fourth, the digit loop. Both integer and float scanners walked digits one at a time, and the loop's exit is a branch that lands on a different digit for every value of a document whose numbers vary in length, so it mispredicted about as often as it ran. The scanners now load a word, light the lanes that are not digits, and fold the digits before the first lit lane in one step, by shifting them up and filling behind them with `'0'` so that the eight-digit fold reads them as a number with leading zeros. A number of one digit costs the same as one of seven, with no loop and no branch on the count. The docs above record a SWAR attempt on the integer path that went slower; that one located the terminator and then still looped over the digits. Integers up to fifteen digits take two of these steps inline, which is where identifiers and millisecond timestamps live and where the previous shape made a call: 13.7 ns a value became 6.4.

Fifth, strings. An escape-free string was scanned a word at a time and then copied in one `memcpy` whose length the compiler could not see, and for the short strings documents are made of, the call cost more than the bytes. The scan now stores each word into spare capacity before the mask says whether it was clean, and counts the stored bytes into the length only as far as the first one that needs escaping. A string of fifteen characters went from 16.4 ns to 11.6.

Per value, on a 10,000-element array of each, before and after:

| | before | after |
|---|---:|---:|
| `f64`, 17 digits | 22.9 ns | 19.1 ns |
| `f64`, short decimal | 16.9 ns | 13.0 ns |
| `u64`, 1-7 digits | 5.9 ns | 5.4 ns |
| `u64`, 13 digits | 13.7 ns | 6.4 ns |
| `i64`, 1-7 digits, random sign | 10.8 ns | 7.3 ns |
| `bool`, random | 4.4 ns | 3.6 ns |
| `String`, 0-30 chars, written | 16.4 ns | 11.6 ns |

Three things were tried on the float writer and measured, and none of them survived. Laying fixed notation out without a branch on the value's shape, as Glaze's table-driven layout does, was slower both as constant-width memory moves and as digits composed in registers with variable shifts: the shapes in these documents are predictable enough that the branches were cheap, and the branchless arithmetic was not. Rendering straight into the writer's spare capacity instead of a scratch array copied in afterwards changed nothing measurable, which retired a theory that the copy was stalling on store forwarding. And loading both words of a float's fraction before deciding on the first, to shorten the cursor's dependency chain, did nothing either. What is left on the float writer is what the section above says: the digit split, which Glaze does with NEON and this crate does with three scalar multiplies per half, and which this crate declines to do with intrinsics.

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

- **No intermediate representation.** A field's bytes are converted exactly once, into the member that will hold them. There is no token stream and no intermediate tree to build and then walk; `Value` is a destination you can ask for, never a stage on the way to a declared type.
- **Keys are hashed at compile time.** The macro picks the cheapest perfect hash that fits your key set, from a single byte comparison up to a full key hash. Both formats look keys up in that one table.
- **Reads reuse what you already own.** Parsing into an existing value refills its buffers instead of reallocating them, which is what makes a loop over records of the same shape settle into doing no allocation at all.
- **BEVE numeric arrays are bulk copies** in both directions when the stored element type matches the destination's, and either form is taken whole: the aligned one is not a slower document to read here. It can also be no copy at all. `to_beve_aligned` pads each payload onto its own element width, and a `Cow<'de, [f64]>` field then points into the document instead of copying out of it, when the document's own address allows it.

[design.md](design.md) has the detail, including the two inlining decisions that mattered most and the places where Rust forced a different answer than the C++ original.
