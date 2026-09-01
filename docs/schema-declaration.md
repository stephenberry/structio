# Schema Declaration: Three Approaches

Glaze gets member names for free. C++26 static reflection (and Glaze's pre-C++26 aggregate trick) lets `glz::read_json` work on a plain struct with **zero annotation**, and `glz::meta<T>` exists only as the opt-in override for renaming, reordering, or describing types that reflection cannot see.

Rust has no reflection at all, at any stage of compilation. There is no stable way to enumerate a struct's fields without the author of the code telling us what they are. So every option below is an analogue of `glz::meta<T>`, not of Glaze's reflection default. The zero-annotation path does not exist in Rust and cannot be built.

What follows is the same struct declared three ways, the tradeoffs, and where each lands on compile time. The compile-time section is measured rather than predicted, and the measurements disagree with what this document originally guessed.

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
- **Nothing to build first.** `macro_rules!` runs inside the compiler frontend as a token-tree rewrite: no process boundary, no serialization, no dynamic library, and nothing extra when cross-compiling. This is a dependency-graph win rather than a wall-clock one; see [compile time](#compile-time).
- **Non-intrusive.** Like `glz::meta<T>`, the declaration lives away from the type definition. You can declare a schema for a type defined in another module, or in a macro, or generated by a build script.
- **No build-graph serialization.** Nothing has to be compiled and linked before your crate can start.
- **Debuggable.** `cargo expand` output is readable, and the macro itself is ordinary Rust source in the crate you already have open.

### Cons

- **Field names are written twice** (once in the struct, once in the `object!` call), so the two can be written inconsistently. What used to be the single real cost of this approach was that they could then *drift*: a field added to the struct and forgotten in `object!` compiled fine and was silently absent from the output, in every format. That no longer compiles. See [the completeness check](schemas.md#a-declaration-is-checked-against-its-type). What remains is that the names are still written twice, which is typing rather than a hazard.
- **Attribute syntax is clunkier.** A per-field option is either positional (`"key" => field`) or a bare marker (`#[required] field`) rather than an argument list like `#[json(rename = "key")]`. A `macro_rules!` matcher can carry a marker and reject a misspelled one, but it cannot parse an arbitrary attribute grammar, so every option has to earn its own place in the field syntax. A *container*-level option has more room: `as "camelCase"` sits in the header, where it costs one extra arm per generics form and nothing at all at a field.
- **The header is the expensive place to put an option.** Only `{`, `[`, `=>`, `,`, `>`, `=`, `:`, `;`, `|`, `as` and `where` may follow a `$ty:ty` fragment, so an optional token after the type has to be one of those and cannot be made optional in the same arm as the thing it precedes. That is why `__declare!` carries six arms for one option over three generics forms. It is a ceiling rather than the base of an exponential: `as` is in that follow set, so a second container option belongs behind the same `as`, where a muncher handles it in one arm each. Inside the braces there is no such restriction, which is why `..` cost two arms and not twelve.
- **Generics need explicit bound restatement** in the macro call, as shown above.
- **Poorer error messages.** A type error inside a macro expansion points at the macro invocation, not the offending field.

---

## B. Hand-rolled `#[derive]`

A second crate, `structio-derive`, with `proc-macro = true`. Written against the raw `proc_macro::TokenStream` with no `syn`, no `quote`, no `proc-macro2`.

```rust
use structio_derive::Structio;

#[derive(Structio, Default)]
#[structio(case = "camelCase")]
struct Accessor {
    #[structio(required, key = "type")]
    component_type: u32,
    byte_offset: u32,
    #[structio(skip)]
    cache: Vec<u8>,
}
```

Generics need no restatement; the derive reads them off the token stream, which is the one thing a `macro_rules!` pattern structurally cannot do.

```rust
#[derive(Structio, Default)]
struct Page<T> {
    items: Vec<T>,
    cursor: Option<String>,
}
```

### Two designs, and the one that was built

A derive can **emit the impls itself**, in which case it can count and so emits a real `match index { 0 => .., 1 => .. }` rather than A's counter chain. It then holds a second copy of everything the schema means: the key list, the required mask, the adapters, the case rules, the element type, the completeness check. Every feature added to `object!` afterwards has to be added here too.

Or it can **emit the `object!` invocation** and stop, in which case it is a front end that understands the declaration *syntax* and none of its semantics.

The second was built, as a prototype, and then removed. What follows is what it measured, since a number beats the estimate this section used to carry.

### What it came to

**493 lines of code** (627 with its documentation), of which roughly half is the token walking `syn` exists to replace and the rest is attributes and emission. It expanded the example above to exactly:

```rust
structio::object!(Accessor as "camelCase" {
    #[required] "type" => component_type,
    byte_offset,
    ..
});
```

It reached `crate`, `case`, `json`, `beve`, `array`, `element` and `bound` on the container and `key`, `required`, `with` and `skip` on fields, renamed a single lifetime to `'de`, and preserved a type parameter's own bounds while adding the format's. `#[structio(skip)]` became the `..` that [the completeness check](schemas.md#a-declaration-is-checked-against-its-type) requires, so a derived declaration cannot drift either.

**Compile cost, measured against the macro it expands to**, 200 structs of 10 fields:

| | Cold leaf | Warm rebuild |
|---|---|---|
| `object!` | 6.56s | 1.72s |
| `#[derive(Structio)]` | 6.95s | 1.71s |

Six percent cold, nothing warm: about 2ms per struct for the bridge, the walk and the reparse. The crate itself built from clean in **0.32s**, against the 5 to 15 seconds `serde_derive` costs with `syn`. Both figures contradict what this section used to say about proc-macro crates being the largest single compile-time cost; that is true of `syn`, not of a proc-macro.

### Why it was not shipped

**A's drift is closed.** Writing the field names once was the argument that mattered, and it is [no longer A's problem](schemas.md#a-declaration-is-checked-against-its-type). What is left is typing.

**It cannot cover enums.** `unit_enum!` and `tagged_enum!` need the shape of each variant's payload, which is a second parser rather than a longer one. A derive that covers structs and not enums makes "two ways to do things" concrete: two styles in one file.

**It is intrusive**, so it can never replace A. No type from another module, a build script, or another macro. It is only ever an alternative.

**It is still 493 lines to keep in step.** Nothing in it duplicates schema semantics, which is the point of the thin design, but it duplicates the schema *syntax*, and every future declaration feature needs an attribute spelled for it.

### Pros

- **Field names written once.** Adding a field to the struct adds it to the schema. Under A the same mistake is now a build error rather than silence, so this is convenience rather than safety.
- **Idiomatic.** This is what every Rust user expects, because it is what `serde` does.
- **Best attribute ergonomics.** `#[structio(key = "...")]` and `#[structio(skip)]` are self-documenting and attach to the field they modify, and they cost nothing at the field syntax the way A's positional forms do.
- **Handles generics and lifetimes** without the user restating them. This is the one thing `macro_rules!` cannot be made to do.
- **Skips const evaluation.** A derive could run `KeyMap::build`'s seed search as native host code and emit the finished table as a literal. The prototype did not, being a front end, but a full derive could, and it is the one place a proc-macro is genuinely better placed.

### Cons

- **It is a dependency.** `cargo tree` shows it, it is built for the host even when cross-compiling, and it is a serialization point in the build graph that a normal rlib is not. This is the real cost, and it is not measured in seconds.
- **Worse spans, not better.** A derive *can* point at the exact field, but the thin design emits text and parses it back, so every error inside the expansion lands on the derive. That is the same place `object!` points today; it is a cost of the thin design rather than of derives.
- **Hand-rolled token parsing is fiddly.** Attribute parsing, generic parameter splitting, and nested generics with `>>` all need care. Half the 493 lines are that.
- **Coverage has to be complete to be worth having.** The forms it refuses have to be refused by name, or the derive becomes a path people hit a wall in.

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

This section originally predicted the ordering from first principles. The predictions were wrong in both directions, so here is the measurement instead: 200 structs of 10 fields each, declared four ways, debug profile, dependencies pre-built, timing the leaf crate alone.

| Declaration | Cold | Warm rebuild |
|---|---|---|
| plain structs, no schema | 0.10s | 0.09s |
| `array!`, both formats (positional, no keys) | 1.35s | 0.40s |
| `json_object!`, one format | 4.07s | 0.87s |
| `#[derive(Serialize, Deserialize)]`, one format | 4.44s | 0.97s |
| `object!`, both formats | 7.52s | 1.58s |

**Per format, a `macro_rules!` declaration and a proc-macro derive cost the same.** The bridge, the token serialization, and the re-parse are all real, and they are all small next to the type checking and codegen of the impls themselves, which both approaches emit identically. Building the whole graph from scratch is a wash too: 7.53s for structio against 7.34s for `serde`, because a 19k-line dependency-free rlib costs about what `syn` plus `serde_derive` cost.

So **the reason to avoid a proc-macro is not compile time.** It is the dependency graph: nothing to audit or vendor, no host cdylib that every downstream crate blocks on, and nothing extra when cross-compiling. Those are worth having and are what the README now claims.

Two of the original predictions are worth correcting explicitly, since both pointed the wrong way:

`KeyMap::build` was expected to dominate and to cost the same everywhere. Priced on its own it is small next to the generated key-matching code, and it is *not* approach-independent: a proc-macro would run the seed search as native host code and emit the finished table as a literal, skipping const evaluation altogether. That is the one place a derive would clearly win.

The counter chain was expected to be a meaningful part of A's cost. Against the same declaration written as a `match`, it is about 5%: 0.21s against 0.13s over 4,000 arms, which is 0.07s inside a 1.58s build. It stays for the expansion-shape reason given above, not because it is free, but it is not where the time goes.

The row this table cannot hold is B against A over the same schema, because that is a different run and the numbers are not comparable across one. It is [in section B](#what-it-came-to), paired: six percent cold, nothing warm.

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

Ship **A** in the core crate as the primary path, with **C** always available underneath as the documented escape hatch. That keeps `structio` at literally zero dependencies and zero proc-macros, and it is the closest analogue to `glz::meta`.

The compile-time half of the original argument for A did not survive measurement and has been struck from it. What is left still holds, and holds for better reasons: the dependency graph, non-intrusiveness, and an expansion a person can read. The one cost that would have argued for B, field drift, is closed by [the completeness check](schemas.md#a-declaration-is-checked-against-its-type).

**B** was built as a prototype, measured, and removed rather than shipped, for the reasons in [why it was not shipped](#why-it-was-not-shipped). It can still be added later as a separate crate without changing a line of the core library, because all three target the identical trait, and it is the way to skip `KeyMap::build`'s const evaluation if that ever becomes the problem. What would argue for it is someone hitting the generics restatement often enough to say so, or a per-field option that will not fit A's positional syntax. Neither has happened.
