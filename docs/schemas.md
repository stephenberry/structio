# Schemas and types

Rust has no reflection, so a struct's field names have to be stated once somewhere. `object!` is where. It turns a field list into the trait impls the parsers and writers need, for every supported format at once. `array!` is its positional counterpart, for structs encoded as arrays rather than as keyed objects.

For *why* this is a `macro_rules!` macro rather than a `#[derive]`, see [schema-declaration.md](schema-declaration.md). This page is about using it.

## Declaring a schema

```rust
#[derive(Default)]
struct Person {
    first_name: String,
    age: u32,
}

structio::object!(Person { first_name, age });
```

That is the whole declaration. `Person` can now be read from and written to both JSON and BEVE.

The macro is invoked at the same scope as the type, not inside it, and it does not modify the struct. Nothing is hidden: the code it generates is the code you would write by hand, and `examples/manual_impls.rs` is that code, spelled out and compiled.

### Keys

Keys default to the field names. Give an explicit key when the encoded name differs from the Rust one:

```rust
structio::object!(Person {
    "first-name" => first_name,
    age,
});
```

The key is a string literal, so it can hold anything the format can carry, including characters that are not valid in a Rust identifier.

Field order in the declaration is the order members are **written**. Reading does not care about order.

### Generics and borrowing

Impl generics go in brackets before the type:

```rust
#[derive(Default)]
struct Page<T> {
    items: Vec<T>,
    cursor: Option<String>,
}

structio::object!([T: structio::ReadWrite + Default] Page<T> { items, cursor });
```

`structio::ReadWrite` is the convenience bound meaning "readable and writable in every format". Use `json::ReadWrite` or `beve::ReadWrite` for a type that is deliberately one format only.

When the type borrows from the input, write the `'de` lifetime yourself. It is the lifetime of the document being parsed:

```rust
#[derive(Default)]
struct Borrowed<'a> {
    name: &'a str,
}

structio::object!(['de] Borrowed<'de> { name });
```

The macro spots a leading `'de` in the bracket list and uses the list verbatim for both halves. Without one, it adds `'de` to the read impls and leaves the write impls alone, since a writer must not declare a lifetime it does not constrain.

### One format only

`object!` generates impls for every format, so **every field's type has to be readable in all of them**. When that is not true, or when you simply do not want the other format's code generated, `json_object!` and `beve_object!` take the same syntax and generate one side:

```rust
#[derive(Default)]
struct Frame<'a> {
    id: u32,
    payload: &'a [u8],
}

structio::beve_object!(['de] Frame<'de> { id, payload });
```

A borrowed `&[u8]` is the case that forces this: BEVE stores a run of bytes verbatim and can hand back a subslice, while JSON has no such representation, so there is no JSON impl to generate.

The shared half, the field list and its compile-time hash, is emitted once either way, so a type declared with `beve_object!` can be given JSON impls by hand later without conflict.

### Positional structs

Some types are encoded as arrays rather than objects: a coordinate, a colour, a row of a table, anything whose field names carry no information the reader does not already have. `array!` declares those. It takes brackets where `object!` takes braces, and the shape of the declaration is the shape of the output:

```rust
#[derive(Default)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}

structio::array!(Vec3 [x, y, z]);
```

`Vec3` now writes as `[1,2,3]` in JSON and as a BEVE generic array of three numbers. There is no renaming syntax, because there are no keys to rename, and declaration order is the whole schema.

It is cheaper than an object in every respect. Nothing is hashed, nothing is compared, no `KeyMap` is built or stored, and the keys are off the wire entirely. A tuple is the same encoding without the names, and goes through the same code, so `(f64, f64, f64)` and the `Vec3` above produce identical bytes in both formats.

#### Homogeneous structs

When every field is the same type, name it in front of the field list, the way an array type names its element:

```rust
#[derive(Default)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

structio::array!(Rgb [u8; r, g, b]);
```

JSON is unchanged. BEVE stores it as a **typed array**: one header for the whole run rather than one per element, and the values as a contiguous block. `Rgb` goes out in five bytes rather than eight, three `f64`s in twenty-six rather than twenty-nine, and three `bool`s in three rather than five, since booleans pack one per bit. The bytes are exactly a slice's, which is also what another implementation writes for its own three-component colour.

The element type is checked against every field, and it has to be `Copy`, since the fields are gathered into a block to be written as one. A type with no typed array of its own, another struct say, falls back to a generic array.

Reading is unaffected: an array-declared struct accepts a generic array or a typed one however it was declared, so naming an element type changes what you write without narrowing what you accept.

What it costs is room to move:

| | `object!` | `array!` |
|---|---|---|
| A field the reader does not know | `UnknownKey`, or skipped under [`SkipUnknown`](options.md#error_on_unknown_keys) | Wrong length, and an error |
| A field the document does not have | Left at its current value, or `MissingKey` under [`RequireKeys`](options.md#error_on_missing_keys) | Wrong length, and an error |
| Fields reordered in the declaration | Changes write order only | Changes what every position means |

Naming an element type tightens this further: the struct is then a run of one type, and changing a field's type changes the whole array's encoding.

An object *can* tolerate a schema that drifts, because a reader matches on names and a key it does not recognize is one it could step over. That is a policy rather than the default: `SkipUnknown` asks for it. An array cannot tolerate drift under any policy, since position is the whole schema. So reach for `array!` when the shape is fixed by something outside your control, and `object!` otherwise.

`json_array!` and `beve_array!` generate one side, exactly as their object counterparts do.

### Enums

An enum's schema is its variant names, and they go on the wire as names rather than as positions, so adding or reordering variants does not change what a document already means.

A variant that carries nothing is written as its name:

```rust
#[derive(Default)]
enum Level {
    #[default]
    Info,
    Warning,
    Error,
}

structio::unit_enum!(Level { Info, Warning, Error });
```

`Level::Warning` writes as `"Warning"` in JSON and as a BEVE string. Names are renamed the way keys are, with `"warning" => Warning`.

A variant that carries a value is written as an object of one member, keyed by the name. Mark it with `(_)`:

```rust
#[derive(Default)]
struct Circle { radius: f64 }
structio::object!(Circle { radius });

#[derive(Default)]
enum Shape {
    #[default]
    Empty,
    Circle(Circle),
    Sides(u32),
}

structio::tagged_enum!(Shape { Empty, Circle(_), Sides(_) });
```

`Shape::Empty` writes as `"Empty"` and `Shape::Circle(..)` as `{"Circle":{"radius":2}}`. The payload's type is not repeated in the declaration; it is already on the enum, and stating it twice would be a second place to keep in step.

Both wire forms are accepted for either kind of variant, so a producer that always writes the object form still round-trips: `Empty` reads back from `"Empty"` and from `{"Empty":null}` alike. What is refused is a name no variant claims, and that is refused under every policy, `SkipUnknown` included. An unknown object key can be stepped over and the object still read; an unknown variant would leave the value itself undecided, so there is nothing to fall back to.

`unit_enum!` will not compile if a variant carries a value. That promise pays for itself in BEVE: the value is a string and can be nothing else, so a `Vec<Level>` is stored as a **string array**, one header for the whole run, and comes out byte for byte what a `Vec<String>` of the same names would. `tagged_enum!` cannot do that even for a declaration that happens to be all unit variants, since a variant carrying a value writes an object. Reading is unaffected either way: a sequence of enums accepts a string array or a generic one however it was written.

Either macro makes extending the enum without extending the declaration a compile error rather than a value that silently writes nothing.

#### One payload, of a type you already declared

A variant carries at most one value. That is the shape a `std::variant<A, B, C>` has, and the one that composes: the payload is an ordinary type, declared with `object!` or `array!` or built in, and the enum adds only the tag. A Rust variant with several fields, or with named fields, is not accepted; give it a struct or a tuple instead.

```rust
#[derive(Default)]
enum Span {
    #[default]
    None,
    Range((u32, u32)),
}
structio::tagged_enum!(Span { None, Range(_) });
```

`Span::Range((1, 5))` writes as `{"Range":[1,5]}`.

BEVE's own type-tag extension is not used for any of this. It is deprecated, and it tags by index, which is the thing an enum's names are here to avoid. A tag is an ordinary one-member object, so everything that walks a document without decoding it — validating, pointing at a field, transcoding to JSON — reaches through a variant with no knowledge of enums at all: `/shape/Circle/radius` is a pointer like any other.

Reading reuses what the destination already holds when it is already the variant being read, so a loop that reads the same variant repeatedly keeps its payload's buffers. Reading a *different* variant replaces the value, which is what changing variants means.

`json_tagged_enum!` and `beve_tagged_enum!` generate one side, exactly as their object counterparts do.

### Writing the impls by hand

Fully supported, and the escape hatch for anything the macro cannot express: computed fields, custom coercions, or wire formats that do not map onto struct members. A type from another crate is a case of its own, and has [two answers](#types-you-do-not-own) that are less work than a hand-written object impl.

There are four impls per format plus one shared `Keys`. See [`examples/manual_impls.rs`](../examples/manual_impls.rs), which is runnable and is quoted verbatim in [schema-declaration.md](schema-declaration.md#c-manual-trait-impls).

The read and write methods are both generic over the [policy](options.md), which an impl forwards on and need not name. An impl that reads keyed data has to apply the key policies itself, since `read_map` cannot know whether its caller's key set is fixed or arbitrary; `Parser::position` and `Parser::rewind` (and their `Reader` twins) are how it reports the failure against the object rather than wherever it happened to notice. BEVE's `WriteObject` has one extra: `count_fields` states how many members `write_fields` will write, since the count goes out before them. An impl that writes every field unconditionally returns `Self::KEYS.len()`.

The array forms are the same count: `ReadArray` and `WriteArray` per format, `Read` and `Write` delegating to them, and one shared `Elements` carrying the length.

## What happens on the way in

| Situation | Behaviour |
|---|---|
| Keys arrive in a different order than declared | Fine. Order is irrelevant to reading. |
| The document has a key you did not declare | Refused, as [`UnknownKey`](options.md#error_on_unknown_keys). Under [`SkipUnknown`](options.md#error_on_unknown_keys) it is skipped instead, whatever it holds, including nested objects and BEVE extensions. |
| A declared field is absent from the document | Left exactly as it was in the destination value. Under [`RequireKeys`](options.md#error_on_missing_keys) it is a [`MissingKey`](options.md#error_on_missing_keys) instead. |
| A member was left out by [`SKIP_NULL`](options.md#skip_null) | Absent, so the row above: the destination keeps what it had, and a `Default` destination gets the `None` back. Writing under `SKIP_NULL` and reading under `RequireKeys` therefore contradict each other. |
| The same key appears twice | The last one wins. |
| A value has the wrong type | An error, never a silent coercion. |

The first three rows are about keys, so they are about `object!`. A positional struct has no keys to arrive out of order, be unknown, or be absent: an array of the wrong length is an error and that is the whole story. An enum has one tag rather than a set of keys, so those rows do not reach it either: a tag naming no variant is an `UnknownVariant` under every policy.

Note that the second and third rows differ, and deliberately. A key you did not declare is a document saying something you have no place to put, which is usually a mistake worth hearing about. A field the document does not mention is a document that is merely quieter than it could have been, and the destination already holds an answer for it.

"Left exactly as it was" is worth dwelling on, because it is what makes `read_into` useful: reading into a fresh `T::default()` gives you defaults for absent fields, and reading into a value you already populated gives you a merge. `RequireKeys` is exactly the policy that gives that up, and the two are meant to be at odds: a patch is a document that leaves members out, so a program that reads patches and a program that reads whole values want different policies rather than different calls.

## Supported types

| | |
|---|---|
| Integers | `u8` `u16` `u32` `u64` `u128` `usize` `i8` `i16` `i32` `i64` `i128` `isize` |
| Floats | `f32` `f64` |
| Other scalars | `bool`, `char`, `()` as `null` |
| Strings | `String`, `&'de str`, `Cow<'de, str>` |
| Sequences | `Vec<T>`, `VecDeque<T>`, `[T; N]`, `HashSet<T>`, `BTreeSet<T>`, `Cow<'de, [T]>` |
| Maps | `HashMap<K, V>`, `BTreeMap<K, V>` |
| Wrappers | `Option<T>`, `Box<T>`, `Rc<T>`, `Arc<T>` |
| Tuples | Up to twelve elements |
| Numeric | `Complex<T>`, `Matrix<T>`, `MatrixRef<'a, T>` (write only) |
| BEVE only | `&'de [u8]` |
| Enums | Declared with `unit_enum!` or `tagged_enum!`. Each variant carries nothing or one value, and the enum and every payload type need `Default` |

`Complex` and `Matrix` are BEVE's two data-carrying extensions, and are stored as those; in JSON they take the encodings they would have had anyway, `[re,im]` and `{"layout":…,"extents":[…],"value":[…]}`, which both also read back from BEVE. A `Complex`'s components are the fixed-width numbers BEVE's class field can name: `f32`, `f64`, and the signed and unsigned integers from 8 through 128 bits. See [BEVE](beve.md#complex-numbers-and-matrices).

Map keys may be strings, `char`, or integers. In JSON an integer key is stringified, as the format requires; in BEVE it is stored as an integer at its own width, with no round trip through text.

A type outside this table can still be a field, through an [adapter](#types-you-do-not-own). Two rules come with one: an adapted container's *elements* need `Default`, and an adapted `Vec` gives up BEVE's bulk read.

### `Default` is required where values are constructed

Any type that has to be *created* during a read needs `Default`: an `Option`'s payload, the new tail of a growing `Vec`, a map's values, and an enum variant's payload, since reading a variant the destination is not already holding has to build one. Types that are only ever read *into* an existing slot do not.

This is the same requirement Glaze places on the types it deserializes, and it is what lets reading reuse the storage a value already holds instead of building a new one and assigning over the top.

### Borrowing out of the input

`&'de str` and `Cow<'de, str>` point directly into the document with no copy.

In JSON this has an edge: a string containing escapes has no representation as a subslice of the input, because the escaped form is what is stored. `&str` reports an error there rather than quietly allocating behind your back. `Cow` accepts both and becomes owned only when it has to. If you do not know whether your input contains escapes, use `Cow`.

BEVE stores strings verbatim with a length prefix and no escaping, so `&'de str` always borrows and this case does not arise. The same is true of `&'de [u8]`.

A run of numbers is the other thing BEVE can hand back whole. `Cow<'de, [f64]>` borrows the block where the document allows it, which needs the [aligned form](beve.md#arrays-a-reader-can-point-at) and a document whose own address a `f64` could live at; where it does not, the field is the copy it would have been anyway. The element type has to be one of the fixed-width numbers, or a `Complex` of one. There is no `&'de [f64]`, only the `Cow`: a field that must borrow would make a program's correctness depend on the address its input happened to be allocated at. In JSON the same field is always the owned half, an array of text being something to build rather than point at.

`Cow<'de, [u8]>` is the case with nothing to satisfy, one-byte elements being aligned wherever they land, so it borrows whatever address the document is at. It differs from `&'de [u8]` in what it accepts: that one takes a run of bytes of either signedness and errors on anything else, where the `Cow` borrows the unsigned run and copies out of anything else a `Vec<u8>` could have read.

## Types you do not own

Rust's orphan rule means you cannot describe a foreign type from your crate the way you can specialize `glz::meta` for any C++ type: neither the trait nor the type is yours. There are two answers, and which one is right depends on how the type is used rather than on what it is.

### Adapters

An **adapter** is a type of your own that says how somebody else's type is read and written. The field keeps its own type; only the encoding of it moves.

```rust
use std::time::Duration;

use structio::{ErrorCode, Options, from_str, json, to_string};

/// The adapter. A unit struct is enough: it is never constructed, only named.
struct Millis;

impl<'de> json::ReadAs<'de, Duration> for Millis {
    fn read<O: Options>(
        value: &mut Duration,
        p: &mut json::Parser<'de, O>,
    ) -> Result<(), ErrorCode> {
        let mut ms = 0u64;
        json::Read::read(&mut ms, p)?;
        *value = Duration::from_millis(ms);
        Ok(())
    }
}

impl json::WriteAs<Duration> for Millis {
    fn write<O: Options>(value: &Duration, w: &mut json::Writer<'_, O>) {
        json::Write::write(&(value.as_millis() as u64), w);
    }
}

#[derive(Default, Debug, PartialEq)]
struct Job {
    id: u32,
    // Still a `Duration`, and still an `Option<Duration>` and a
    // `Vec<Duration>`: only the encoding of them moved.
    elapsed: Duration,
    timeout: Option<Duration>,
    retries: Vec<Duration>,
}

structio::json_object!(Job {
    id,
    "elapsed_ms" => elapsed as Millis,
    timeout as Option<Millis>,
    retries as Vec<Millis>,
});
```

`Millis` is never constructed. It exists to be named at the field site and to carry the impls, and because it is a type rather than a module of functions, three things follow.

**It composes.** `Option<Millis>`, `Vec<Millis>`, `[Millis; N]`, `HashMap<Same, Millis>` and their nestings are adapters over the corresponding containers, each mirroring that container's own impl down to which allocations a read reuses. `structio::Same` is the identity adapter, for a position that wants the type's own impl inside one that does not: a `HashMap<Same, Millis>` adapts the values and leaves the keys alone. A whole-container adapter is equally possible — `blob as Hex` over a `Vec<u8>` writes one string rather than an array — and the two can sit in the same declaration.

**It is per format.** `object!` asks for `json::ReadAs`, `json::WriteAs`, `beve::ReadAs` and `beve::WriteAs`; `json_object!` asks for the first pair alone. An adapter that only makes sense in one format is a `json_object!` declaration, exactly as a `&[u8]` field is a `beve_object!` one. The flip side is that one name at the field site now covers two encodings, and nothing checks that they agree: an adapter whose JSON half writes a string and whose BEVE half writes an integer is legal and invisible at the declaration. Keeping the two halves saying the same thing is the adapter author's job.

**Somebody else can publish it.** The impls are on the adapter, which is local to whoever writes them, so a third crate may ship `pub struct Rfc3339;` with `impl structio::json::ReadAs<'_, DateTime<Utc>> for Rfc3339` and everyone downstream just names it. That is the property that makes a foreign type painless, and it arrives without this crate depending on anything.

Adapters are not orphan-rule relief. The impls are still written by hand, once per adapter and target type. What changes is that they are written once and named from any field in any crate, instead of once per wrapper with the wrapper spreading through your API.

### Newtypes

The older answer, and still the right one twice over.

```rust
#[derive(Default)]
struct Timestamp(other_crate::DateTime);
```

**When the foreign type has no `Default`.** Reading constructs values in the places [listed above](#default-is-required-where-values-are-constructed), and an adapted container is one of them: `Vec<Rfc3339>` over a `Vec<DateTime>` needs `DateTime: Default`, and there is nowhere on the adapter to put that impl. A newtype discharges it with one line. This is the sharp edge of the mechanism, and it bites at element positions rather than at the struct's own fields — only `Box<A>` and `[A; N]` avoid it, having no element to build.

**When the type appears in many structs.** `at as Rfc3339` is per field. A newtype is written once and then *is* the type everywhere, at the cost of `.0` at every use.

The two combine: a newtype is a type you own, so it can carry ordinary impls, and an adapter can target it like anything else.

### What an adapter costs in BEVE

An adapted contiguous sequence keeps its typed array on the way out — `Vec<Same>` over a `Vec<f64>` is byte for byte the field it wraps — but gives it up on the way in. The bulk read copies a whole numeric block into a `Vec<T>` in one `memcpy`, and it can only do that by knowing the element type, which is exactly what an adapter replaces. A numeric field that wants that path should stay unadapted.
