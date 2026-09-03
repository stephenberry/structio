# Enums

An enum's schema is its variant names, and they go on the wire as names rather than as positions. Adding a variant, or reordering the ones already there, does not change what a document already means.

That is the one design decision everything else on this page follows from. BEVE has a type-tag extension that would do the job in fewer bytes, and it is deliberately not used: it is deprecated, and it tags by index, which is precisely the thing names are here to avoid.

## Two wire forms for a wrapping tag

These are the two a variant takes when the name wraps the payload, which is what `unit_enum!` and a clause-free `tagged_enum!` write. [Internal tagging](#internal-tagging) moves the name inside the payload's object and has a wire form of its own; the rest of this page is about these two until then.

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

Both macros also take a [case rule](schemas.md#case-rules) after the type, which renames every variant the declaration does not spell out. The rule splits a name into words, and a Rust variant is spelled with capitals rather than underscores, so that is where it splits:

```rust
structio::unit_enum!(Mode as "kebab-case" { ReadOnly, ReadWrite, HTTPProxy });
```

writes `"read-only"`, `"read-write"` and `"http-proxy"`.

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

## Internal tagging

The two forms above are *external* tagging: the name wraps the payload. A **tag clause**, `as tag "kind"`, puts the name **inside** the payload's object instead, as a member beside the payload's own:

```rust
#[derive(Default)]
struct Circle { radius: f64 }
structio::object!(Circle { radius });

#[derive(Default)]
enum Shape {
    #[default]
    Empty,
    Circle(Circle),
}

structio::tagged_enum!(Shape as tag "kind" { Empty, Circle(_) });
```

| The variant | No tag clause | `as tag "kind"` |
|---|---|---|
| Carries nothing | `"Empty"` | `{"kind":"Empty"}` |
| Carries a value | `{"Circle":{"radius":1}}` | `{"kind":"Circle","radius":1}` |

This is the convention most JSON APIs settled on, and the one a C++ Glaze `std::variant` with a declared tag produces. It is the only form here that a deduced variant can be made to agree with: external tagging has nowhere to put the payload's own keys.

### The tag has to come first

**A document whose object begins with any other key is `ExpectedTag`.** This crate reads in one pass with no lookahead and no buffering, so the member deciding which variant is being read has to arrive before the members whose meaning it decides. Finding a tag further in means holding the object somewhere or walking it twice, and neither is a thing this crate does.

The restriction is on reading. Writing always puts the tag first, so a value this crate wrote reads back unconditionally, and so does one from any producer that emits its tag first — the conventional ordering, and what a declaration-ordered serializer does by default. A producer that puts it last is refused, loudly and with the offending key's position, rather than misread:

```rust
from_str::<Shape>(r#"{"kind":"Circle","radius":1}"#)?;  // reads
from_str::<Shape>(r#"{"radius":1,"kind":"Circle"}"#);   // ExpectedTag, at "radius"
```

`ExpectedTag` also covers an object with no members and a tag whose value is not a string. Which of the three it was is not distinguished, because distinguishing them is the search being refused. A tag that *is* first and names nothing is `UnknownVariant` instead: it was found, and its value is not a variant.

### What a payload may be

An object, and nothing else. The variant's members share one object with the tag, so a payload with no members of its own has nowhere to go: `Sides(u32)` is a compile error naming `Keys`, then `WriteObject` and its neighbours, not a runtime surprise. Declare such a payload as a struct, or drop the clause: external tagging takes any payload because it gives it an object of its own.

A variant carrying nothing is written as the tag alone. Members beside it are unknown members and meet the reader's policy exactly as a struct's would be: refused under `Standard`, stepped over under `SkipUnknown`.

### A tag cannot be a payload's field

The tag shares one object with the payload's members, so a tag whose name is also a payload field would write that name twice:

```json
{"kind":"Config","kind":"debug","level":3}
```

structio would read this back, taking the first member as the tag. **Nothing else would.** A last-wins parser — `JSON.parse`, and Glaze — sees `kind` as `"debug"` and the variant is gone. The field is unreadable here too: the tag is consumed before the payload's members are reached, so under `RequireKeys` it could never be filled.

**This is a compile error**, so the shape above cannot be written:

```
error[E0080]: evaluation panicked: structio: the tag of an internally tagged enum is also
a field of this variant's payload. ...
  --> src/main.rs:7:1
   |
 7 | structio::tagged_enum!(Setting as tag "kind" { Off, Config(_) });
   | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
   |
note: inside `assert_tag_not_a_field::<Debugging, Setting>`
```

The payload's type is absent from the declaration, so the check reaches it through the variant's constructor, which is a value of type `fn(Payload) -> Self`. What it compares are *wire* names, so a collision that exists only after a case rule has run — a field `kind_of` under `"camelCase"` against a tag `"kindOf"` — is caught as well, and that one is invisible in the Rust source.

A declaration with no generics is refused by `cargo check`. A generic one has no payload keys until it is instantiated, so it is refused when the crate is built, the same tier as a `Keys::REQUIRED` overflow. A variant carrying nothing is never checked: it shares its object with no members.

Everything else on this page carries over, the clause being an addition to `tagged_enum!` rather than a declaration of its own. Renaming, case rules (which apply to the variant names, never to the tag key, that being a document key rather than a variant), generics, borrowed payloads, reading into an existing value, and `json_tagged_enum!` / `beve_tagged_enum!`, which take the clause too, all work as they do above. And because the result is an ordinary object, the rest of the crate needs to know even less about it than it does about an external tag: a pointer reaches `/kind` and `/radius` in the same object, with no enum-shaped step in the path.

## See also

- [Schemas and types](schemas.md) for structs, the supported type set, and types you do not own.
- [Options](options.md) for the policies above.
- [Errors](errors.md) for what each code means and how to render one.
