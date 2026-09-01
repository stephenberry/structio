# Schema Declaration: Three Approaches

Glaze gets member names for free. C++26 static reflection (and Glaze's pre-C++26 aggregate trick) lets `glz::read_json` work on a plain struct with **zero annotation**, and `glz::meta<T>` exists only as the opt-in override for renaming, reordering, or describing types that reflection cannot see.

Rust has no reflection at all, at any stage of compilation. There is no stable way to enumerate a struct's fields without the author of the code telling us what they are. So every option below is an analogue of `glz::meta<T>`, not of Glaze's reflection default. The zero-annotation path does not exist in Rust and cannot be built.

What follows is the same struct declared three ways, the tradeoffs, and where each lands on compile time.

The running example:

```rust
#[derive(Default)]
struct Person {
    first_name: String,
    age: u32,
    friends: Vec<String>,
}
```

---

## A. `macro_rules!`

A declarative macro shipped inside the `structio` crate itself. No separate crate, no proc-macro, no dependency.

```rust
use structio::object;

#[derive(Default)]
struct Person {
    first_name: String,
    age: u32,
    friends: Vec<String>,
}

object!(Person { first_name, age, friends });
```

Renaming keys, when the encoded name differs from the Rust name:

```rust
object!(Person {
    "first-name" => first_name,
    age,
    friends,
});
```

Marking a member the document has to carry:

```rust
object!(Person {
    #[required] "first-name" => first_name,
    age,
    friends,
});
```

Nested types compose with no ceremony:

```rust
#[derive(Default)]
struct Team { lead: Person, roster: Vec<Person> }

object!(Team { lead, roster });
```

Generic and borrowing types take their impl generics in brackets, since a
`macro_rules!` pattern cannot tell one from the other. Write `'de` yourself when
the type borrows from the input; it is the lifetime of the document.

```rust
#[derive(Default)]
struct Borrowed<'a> { name: &'a str }
object!(['de] Borrowed<'de> { name });

#[derive(Default)]
struct Page<T> { items: Vec<T>, cursor: Option<String> }
object!([T: structio::ReadWrite + Default] Page<T> { items, cursor });
```

### What it expands to

Five small impls. Abbreviated, and with the paths shortened:

```rust
impl Keys for Person {
    const KEYS: &'static [&'static str] = &["first_name", "age", "friends"];
    const MAP: &'static KeyMap = &KeyMap::build(Self::KEYS);
}

impl<'de> ReadObject<'de> for Person {
    #[inline]
    fn read_field(&mut self, index: usize, p: &mut Parser<'de>) -> Result<bool, ErrorCode> {
        let mut i = 0usize;
        if index == i {
            if !p.match_key("first_name") { return Ok(false); }
            p.colon()?;
            Read::read(&mut self.first_name, p)?;
            return Ok(true);
        }
        i += 1;
        // ... one arm per field ...
        Ok(false)
    }
}

impl WriteObject for Person {
    #[inline]
    fn write_fields(&self, w: &mut Writer) {
        w.member("\"first_name\":", &self.first_name);
        w.member("\"age\":", &self.age);
        w.member("\"friends\":", &self.friends);
    }
}

// plus `Read` and `Write`, each delegating to the object form.
```

Three details are worth pointing out, because they are not arbitrary.

`match_key` is what makes the perfect hash safe. `index` comes from a hash, so it is only a *candidate*; an unknown key can collide with an occupied bucket. Confirming it here, where the key is a literal and `key.len()` is therefore a constant, means the comparison inlines to a fixed-size compare rather than a call to `memcmp`. Returning `false` rather than an error lets the caller treat the member as unknown, which under the default is an `UnknownKey` and under [`SkipUnknown`](options.md#error_on_unknown_keys) is a member stepped over. Either way the decision is the caller's, not this function's.

Each member writes an unconditional trailing comma, and the caller overwrites the last one with `}`. No field has to ask whether it is first. The `"key":` prefix is assembled at compile time, so writing a member is one copy of a constant string.

`#[inline]`, not `#[inline(always)]`. `read_field` holds the parser for every field, so forcing it inline duplicates a whole nested struct's parser into each field arm of its parent, recursively. Getting this wrong cost a factor of two to three on the benchmark; see [design.md](design.md).

The `let mut i = 0; if index == i { .. } i += 1;` chain is not a stylistic choice. `macro_rules!` cannot count, so it cannot emit `match index { 0 => .., 1 => .. }` directly. The usual workarounds (recursive token accumulation) expand quadratically in the number of fields and are exactly the kind of thing that makes Rust macros slow to compile. The counter chain sidesteps all of it: `i` const-folds to `0, 1, 2, ...`, and LLVM turns the resulting comparison chain into the same jump table a `match` would have produced. Identical machine code, linear expansion.

### Pros

- **Zero dependencies, zero extra crates.** Satisfies the no-dependency constraint absolutely.
- **Fastest of the three to expand.** `macro_rules!` runs inside the compiler frontend as a token-tree rewrite. No process boundary, no serialization, no dynamic library.
- **Non-intrusive.** Like `glz::meta<T>`, the declaration lives away from the type definition. You can declare a schema for a type defined in another module, or in a macro, or generated by a build script.
- **No build-graph serialization.** Nothing has to be compiled and linked before your crate can start.
- **Debuggable.** `cargo expand` output is readable, and the macro itself is ordinary Rust source in the crate you already have open.

### Cons

- **Field names are written twice** (once in the struct, once in the `object!` call). They can drift. A field added to the struct and forgotten in `object!` compiles fine and is silently absent from the output, in every format. This is the single real cost of this approach.
- **Attribute syntax is clunkier.** A per-field option is either positional (`"key" => field`) or a bare marker (`#[required] field`) rather than an argument list like `#[json(rename = "key")]`. A `macro_rules!` matcher can carry a marker and reject a misspelled one, but it cannot parse an arbitrary attribute grammar, so every option has to earn its own place in the field syntax. A *container*-level option has more room: `as "camelCase"` sits in the header, where it costs one extra arm per generics form and nothing at all at a field.
- **Generics need explicit bound restatement** in the macro call, as shown above.
- **Poorer error messages.** A type error inside a macro expansion points at the macro invocation, not the offending field.

---

## B. Hand-rolled `#[derive]`

A second crate, `structio-derive`, with `proc-macro = true`. Written against the raw `proc_macro::TokenStream` with no `syn`, no `quote`, no `proc-macro2`. Parsing a struct definition well enough to pull out field names, types, and `#[json(...)]` attributes is a few hundred lines of hand-written token walking.

```rust
use structio::Structio;

#[derive(Structio, Default)]
struct Person {
    #[json(key = "first-name")]
    first_name: String,
    age: u32,
    #[json(skip)]
    cache: Vec<u8>,
}
```

Generics need no restatement; the derive reads them off the token stream:

```rust
#[derive(Structio, Default)]
struct Page<T> {
    items: Vec<T>,
    cursor: Option<String>,
}
```

It expands to exactly the same impls as approach A, except that it *can* count, so it emits a real `match index { 0 => .., 1 => .., .. }` instead of the counter chain.

### Pros

- **Field names written once.** Adding a field to the struct automatically adds it to the schema. This eliminates the entire class of drift bugs that A is exposed to.
- **Idiomatic.** This is what every Rust user expects, because it is what `serde` does. Zero learning curve.
- **Best attribute ergonomics.** `#[json(key = "...")]`, `#[json(skip)]`, `#[json(default)]` are self-documenting and attach to the field they modify.
- **Handles generics, lifetimes, and `where` clauses** without the user restating them.
- **Better spans.** Errors can be pointed at the exact field.

### Cons

- **It is a dependency**, even if workspace-internal. `structio` would carry `structio-derive` in its dependency tree, and `cargo tree` shows it.
- **Proc-macro crates are the largest single compile-time cost in the Rust ecosystem.** Even with no `syn`, the crate must be compiled *and fully linked into a host dynamic library* before any dependent crate can begin. This is a hard serialization point in the build graph: `cargo` cannot pipeline past it the way it can past a normal `rlib`.
- **Always built for the host,** even when cross-compiling. On a cross build you pay for two target configurations.
- **Per-expansion cost is higher.** Every `#[derive(Structio)]` serializes tokens across the proc-macro bridge, runs your parser, and serializes tokens back, and the result is then re-parsed by the frontend. `macro_rules!` does none of that.
- **Hand-rolled token parsing is genuinely fiddly.** Attribute parsing, generic parameter splitting, `where` clause handling, and nested generic types with `>>` all need care. `syn` exists because this is annoying, and we are declining to use `syn`.
- **Harder to debug.** Failures surface as compiler panics or malformed output rather than as readable Rust.

---

## C. Manual trait impls

No macros. The user writes the impl.

```rust
use structio::json::{Parser, Read, ReadObject, Write, WriteObject, Writer};
use structio::{ErrorCode, KeyMap, Keys, Options};

#[derive(Default)]
struct Person {
    first_name: String,
    age: u32,
    friends: Vec<String>,
}

impl Keys for Person {
    const KEYS: &'static [&'static str] = &["first_name", "age", "friends"];
    const MAP: &'static KeyMap = &KeyMap::build(Self::KEYS);
}

impl<'de> ReadObject<'de> for Person {
    fn read_field<O: Options>(
        &mut self,
        index: usize,
        p: &mut Parser<'de, O>,
    ) -> Result<bool, ErrorCode> {
        // The index came from a hash, so confirm the key before using it.
        if !p.match_key(Self::KEYS[index]) {
            return Ok(false);
        }
        p.colon()?;
        match index {
            0 => Read::read(&mut self.first_name, p)?,
            1 => Read::read(&mut self.age, p)?,
            2 => Read::read(&mut self.friends, p)?,
            _ => return Ok(false),
        }
        Ok(true)
    }
}

impl WriteObject for Person {
    fn write_fields<O: Options>(&self, w: &mut Writer<'_, O>) {
        w.member("\"first_name\":", &self.first_name);
        w.member("\"age\":", &self.age);
        w.member("\"friends\":", &self.friends);
    }
}

impl<'de> Read<'de> for Person {
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> Result<(), ErrorCode> {
        p.read_object(self)
    }
}
impl Write for Person {
    fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
        w.write_object(self)
    }
}
```

Note that `Self::KEYS[index]` is a runtime index into a slice, so its length is
not a constant and the comparison becomes a `memcmp` call. Writing the arms out
with literal keys, as the macro does, is faster; this shorter form trades a
little speed for readability.

This path exists no matter which of A or B is chosen, because it is the trait every approach targets. It is also the escape hatch for anything the macros cannot express: computed fields, custom coercions, wire formats that do not map cleanly onto struct members.

### Pros

- **Absolute control.** Any read or write behavior is expressible.
- **Fastest to compile,** trivially, because there is no expansion step at all.
- **Nothing hidden.** What you read is what runs.
- **The natural target for the other two.** Keeping this readable keeps `cargo expand` readable.

### Cons

- **The index-to-field correspondence is unchecked.** `KEYS[0]` must line up with arm `0` in `read_field` and with the first `w.member` in `write_fields`, across three separate places. Getting this wrong produces a library that silently reads the wrong data into the wrong field. This is a far worse failure mode than A's missing-field drift.
- **Unusable for wide structs.** A 40-field config type is 120 lines of hand-maintained bookkeeping.
- **Verbose enough that people will avoid the library.**

---

## Compile time

All three produce byte-identical generated code, so **runtime performance is exactly the same in all three cases**. The difference is entirely in the compiler frontend.

There are two components worth separating:

**One-time cost (paid once per build, regardless of how many structs you declare):**

| Approach | One-time cost |
|---|---|
| C. Manual | none |
| A. `macro_rules!` | none (the macro is part of the `structio` rlib you are already building) |
| B. `#[derive]` | compile + link `structio-derive` as a host cdylib, and every dependent crate blocks on it |

Rough magnitude for B: a hand-rolled proc-macro with no `syn` is a small crate, so on the order of a second, but the build-graph serialization it forces is often the more significant effect on a wide dependency graph. For contrast, `serde_derive` with `syn` is routinely 5 to 15 seconds and is the reason `serde` dominates Rust build-time complaints.

**Per-struct cost:**

| Approach | Per-struct expansion | Notes |
|---|---|---|
| C. Manual | zero | nothing to expand |
| A. `macro_rules!` | very low | in-process token-tree substitution, linear in field count with the counter-chain design |
| B. `#[derive]` | low but higher than A | token serialization across the bridge, parse, generate, serialize back, re-parse |

**Cost shared by all three, and probably the dominant one:**

The compile-time perfect-hash construction (`KeyMap::build`) is a `const fn` that searches for a seed producing a collision-free table, exactly as Glaze's `make_keys_info` does in `consteval`. Rust's const evaluator (miri-based) is considerably slower than a C++ compiler's `consteval` engine. This cost is identical across A, B, and C, and for a struct with many keys it will likely exceed the macro expansion cost by an order of magnitude. If compile time becomes a problem, that is where to look, not at the macro choice.

**Ordering, fastest first: C, then A, then B.** The gap between C and A is small. The gap between A and B is real but modest in absolute terms, and consists mostly of the fixed build-graph cost rather than per-struct work.

---

## Which is closest to `glz::meta`?

**A and C, jointly. Not B.**

`glz::meta<T>` is a template specialization: a separate, out-of-line declaration that describes a type from the outside. Its defining property is that it is **non-intrusive**. You can specialize `glz::meta` for a type you do not own, from a header you control, without touching the original definition. That is why it works for third-party library types.

A Rust `impl structio::json::ReadObject for Person` is the direct structural analogue: an out-of-line block that describes a type from the outside. `object!(Person { .. })` is that same impl with the boilerplate removed, in the same spirit as a `GLZ_META`-style convenience macro over a hand-written specialization.

`#[derive]` is the odd one out. It is **intrusive**: it must be written on the type definition itself. That makes it structurally closer to an in-class `static constexpr auto glaze_meta = ...` member than to `glz::meta<T>`, and it means it fundamentally cannot describe a type you do not own.

### One important asymmetry

C++ lets you specialize `glz::meta<T>` for *any* type, including `std::` types and third-party types. Rust's orphan rule does not: you cannot implement a trait you do not own for a type you do not own. So none of the three approaches describes a foreign type directly.

What closes most of the gap is a fourth thing, which C++ does not need: a field may name an **adapter**, a type of your own carrying the impls for somebody else's type.

```rust
struct Millis;                                       // the adapter, never constructed
impl<'de> json::ReadAs<'de, Duration> for Millis { /* ... */ }
impl json::WriteAs<Duration> for Millis { /* ... */ }

json_object!(Job { id, elapsed as Millis });         // the field is still a `Duration`
```

Both halves of that are yours, so it is legal, and the struct's own definition is untouched. It is a `json_object!` because `Millis` has only the JSON pair; giving it the two BEVE impls as well would make it an `object!`. Adapters compose through containers, are per format, and can be published by a third crate for everyone else to name. See [Types you do not own](schemas.md#types-you-do-not-own).

A newtype wrapper is still the answer where an adapter cannot reach: a foreign type with no `Default` cannot be an adapted container's element, and a type used across many structs is better named once than adapted at every field. This is a Rust language limitation rather than a design choice, and it is one place where a Rust port does not fully reach parity with Glaze's flexibility.

---

## What was built

**A**, with **C** available underneath. `structio` has zero dependencies and no proc-macro crate.

## Recommendation

Ship **A** in the core crate as the primary path, with **C** always available underneath as the documented escape hatch. That keeps `structio` at literally zero dependencies and zero proc-macros, honoring both the no-dependency and fast-compile constraints, and it is the closest analogue to `glz::meta`.

If the field-name duplication in A proves annoying in real use, **B** can be added later as an optional `derive` feature on a separate crate without changing a single line of the core library, because all three target the identical trait. That decision does not have to be made now, and deferring it costs nothing.
