# Enums

An enum's schema is its variant names, and they go on the wire as names rather than as positions. Adding a variant, or reordering the ones already there, does not change what a document already means.

That is the one design decision everything else on this page follows from. BEVE has a type-tag extension that would do the job in fewer bytes, and it is deliberately not used: it is deprecated, and it tags by index, which is precisely the thing names are here to avoid.

## Two wire forms, and no third

| The variant | Is written as | Example |
|---|---|---|
| Carries nothing | Its name, as a string | `"Empty"` |
| Carries a value | An object of one member, keyed by the name | `{"Sides":6}` |

Both formats agree on which is which. In BEVE the first is a string and the second is a one-member object, so a tag is an ordinary object to everything that walks a document.

## Declaring one

`unit_enum!` is for an enum whose variants all carry nothing. It will not compile if one of them does, which is what lets its wire form be a string and stay one:

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

`tagged_enum!` is for an enum where at least one variant carries a value. Mark those with `(_)`:

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

The payload's type is not repeated in the declaration. It is already on the enum, and stating it twice would be a second place to keep in step.

Either macro makes extending the enum without extending the declaration a compile error, rather than a variant that silently writes nothing.

### Renaming

A name on the wire may differ from the Rust one, written the way a key is:

```rust
structio::tagged_enum!(Event {
    "connected" => Connected,
    "log" => Log(_),
    "shape" => Shape(_),
});
```

### Generics and borrowing

Both macros take a generics list in brackets, exactly as `object!` does:

```rust
// A payload that borrows out of the document rather than copying from it.
structio::tagged_enum!(['de] Ref<'de> { Nothing, Text(_) });

// A parameter with a bound.
structio::tagged_enum!([T: structio::ReadWrite + Default] Message<T> { Ping, Data(_) });

// A unit enum's parameter can only be a const: Rust will not take a type or
// lifetime parameter that no variant uses, and a variant that used one would
// be carrying a value.
structio::unit_enum!([const N: usize] Slot<N> { Free, Taken });
```

`json_tagged_enum!` and `beve_tagged_enum!` generate one side only, as their `object!` counterparts do.

## One payload, of a type you already declared

A variant carries at most one value. That is the shape a `std::variant<A, B, C>` has, and the one that composes: the payload is an ordinary type, declared with `object!` or `array!` or built in, and the enum adds only the tag.

A Rust variant with several fields, or with named fields, is not accepted. Give it a struct or a tuple instead:

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

## What reading accepts

The two forms are **not** interchangeable, and the asymmetry runs one way only:

| Written | `Empty` (carries nothing) | `Sides` (carries a value) |
|---|---|---|
| `"Empty"` / `"Sides"` | Accepted | **`ExpectedBrace`** (JSON), `ExpectedObject` (BEVE) |
| `{"Empty":null}` / `{"Sides":6}` | Accepted | Accepted |

A variant carrying nothing reads back from either form, so a producer that always writes the object form still round-trips. A variant carrying a value has no bare form: the name alone leaves the value missing, and there is nothing to put there.

That refusal is deliberately *not* `UnknownVariant`. The name was recognized; what was absent was the value under it, and telling those two apart is what makes the error worth reading.

Under the object form, the member of a variant carrying nothing has to be `null` specifically. `{"Empty":0}` is `ExpectedNull`.

## What reading refuses

| Code | When |
|---|---|
| `UnknownVariant` | A name no variant claims: `"Round"`, or `{"Round":1}` |
| `ExpectedVariant` | An object that is not exactly one member (`{}`, or two tags at once), or a value that is not an object or a string at all (`1`, `[]`, `null`, `true`) |
| `ExpectedBrace` / `ExpectedObject` | A payload-carrying variant written bare, as above |
| `ExpectedNull` | The object form of a unit variant, holding something other than `null` |
| `UnexpectedEnd` | The document stopped early, including in the middle of a name |

**An unknown variant is refused under every policy, `SkipUnknown` included.** This is the one place that policy does not reach, and the reason is structural rather than strict: an unknown object *key* can be stepped over and the object still read, because the rest of the members still describe the value. An unknown *variant* leaves the value itself undecided. There is nothing to fall back to and nothing to skip to.

**A truncated document is distinguished from one holding the wrong thing.** `"Empty` and `{"Round` both fail as `UnexpectedEnd`, not as `UnknownVariant`: matching a name fails identically whether the name ran out or was never a name, so the reader walks it to its closing quote before deciding which happened.

**A name that merely hashes like a real one is still refused.** The compile-time table proposes a variant and the name itself confirms it, so `"Emptz"` and `"Sider"` reach the comparison and are thrown out by it, and `"Empt"` -- a prefix of a real name -- is not that name either. All three are `UnknownVariant`.

**The error points at the name.** In `  {"Round":1}` the offset is 4: the name, not the brace before it and not the value after it.

## Policies

The tag object is an ordinary object, so a [write policy](options.md) lays it out like one:

```rust
to_string_with::<Pretty, _>(&Shape::Circle(Circle { radius: 1.0 }))
// {
//   "Circle": {
//     "radius": 1
//   }
// }
```

Two policies meet a tag and deliberately stop short of it:

**`SkipNull` does not reach the tag.** `Maybe::Value(None)` writes as `{"Value":null}` even under `SkipNull`, because dropping that member would leave `{}`, which names no variant at all. That is a different value, not a shorter spelling of this one.

**`RequireKeys` reaches the payload and not the tag.** The tag object has one member, which is the variant; it is not a struct and has no keys to require. The payload underneath still has its own, so `{"Circle":{}}` is `MissingKey` while `{"Circle":{"radius":1}}` reads.

**`AllowComments` puts comments wherever whitespace goes**, the tag's braces and colon included.

## BEVE

**A run of unit variants is a string array.** A unit enum's value is a string and can be nothing else, so `Vec<Level>` is stored the way `Vec<String>` is: one header for the whole run rather than one per element, byte for byte identical to the same names written as strings.

`tagged_enum!` cannot do that, even for a declaration that happens to be all unit variants, since a variant carrying a value writes an object. Its runs stay generic arrays.

Reading is unaffected either way. A sequence of enums accepts a string array or a generic one, however it was written.

## Reading into an existing value

Reading reuses what the destination already holds when it is already the variant being read, so a loop over records of the same variant keeps its payload's buffers:

```rust
let mut shape = Shape::Label(String::with_capacity(64));
structio::read_into(&mut shape, r#"{"Label":"hello"}"#)?;   // keeps the capacity
```

Reading a *different* variant replaces the value, which is what changing variants means.

## What the rest of the crate makes of a tag

A tag is a one-member object and nothing more, so every walk that does not decode reaches through one with no knowledge of enums at all. `validate_beve` accepts it, `beve_to_json` rewrites it, and a JSON Pointer steps through the name like any other key:

```rust
structio::from_beve_at::<f64>(&bytes, "/shape/Circle/radius")?;
```

Tags stream like any other value, in either format, including across a chunk boundary that cuts one in half.

## See also

- [Schemas and types](schemas.md) for structs, the supported type set, and types you do not own.
- [Options](options.md) for the policies above.
- [Errors](errors.md) for what each code means and how to render one.
