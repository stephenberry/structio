# The derive

`#[derive(Structio)]` declares a type's schema from its definition. It is a front end to the declaration macros in [Schemas and types](schemas.md) and [Enums](enums.md): it reads the struct or enum, translates its attributes to the macro's syntax, and emits the `object!`, `array!`, `unit_enum!` or `tagged_enum!` invocation you would have written. The impls, the key map, the required-field mask, the completeness check and every rule about what is accepted on the wire are the macro's own. A derived type and a declared type are the same code.

It is optional. Enable the `derive` feature:

```toml
[dependencies]
structio = { version = "0.3", features = ["derive"] }
```

The feature is off by default, so a crate that does not enable it builds structio as it always has: no dependencies and no proc-macro. The macros are not deprecated. They remain the only way to describe a type from another crate, and the derive changes nothing about what they accept.

## A struct

```rust
#[derive(Default, structio::Structio)]
#[structio(rename_all = "camelCase")]
struct Camera {
    #[structio(required)]
    focal_length: f64,
    #[structio(rename = "iso")]
    sensitivity: u32,
    #[structio(skip)]
    cache: Vec<u8>,
}
```

expands to exactly

```rust
structio::object!(Camera as "camelCase" {
    #[required] focal_length,
    "iso" => sensitivity,
    ..
});
```

The struct's own `Default` is still required, for the reason it is required of a declared type: structio reads into an existing value, and the entry points build that value before reading. The derive does not generate it.

## Attributes

Every attribute maps onto one piece of the macro syntax. Nothing here is a second codec.

### On the type

| Attribute | Expands to |
|---|---|
| `rename_all = "camelCase"` | `as "camelCase"`. The rules are the [case rules](schemas.md#case-rules): `lowercase`, `UPPERCASE`, `PascalCase`, `camelCase`, `snake_case`, `SCREAMING_SNAKE_CASE`, `kebab-case`, `SCREAMING-KEBAB-CASE`. An explicit `rename` wins over the rule. |
| `tag = "kind"` | `as tag "kind"`, an [internally tagged enum](enums.md#internal-tagging). Enums only. |
| `array` | `array!` rather than `object!`: the struct is written as a positional array. |
| `array, element = "u8"` | `array!(T [u8; ..])`, the [homogeneous form](schemas.md#homogeneous-structs) that BEVE writes as one typed array. |
| `json` / `beve` | `json_object!`, `json_array!`, `json_tagged_enum!` or the `beve_` counterpart: impls for one format only. |
| `crate = "path"` | The path to structio where it is re-exported under another name. The default is `::structio`. |

### On a field

| Attribute | Expands to |
|---|---|
| `rename = "key"` | `"key" => field` |
| `skip` | The field is left out of the declaration, and the declaration ends in `..`. Not on the wire in either direction. |
| `required` | `#[required] field`: absence is `MissingKey` under every policy. |
| `with = "Adapter"` | `field as Adapter`, an [adapter](schemas.md#types-you-do-not-own) for a type this crate does not describe. Composes as a type does: `with = "Vec<Millis>"`. |

On a positional struct only `skip` applies. A key, a required marker or an adapter on an element is refused, because `array!` takes none of them: an element is found by position and required by the array's length.

### On a variant

| Attribute | Expands to |
|---|---|
| `rename = "name"` | `"name" => Variant` |

## Generics

The one thing a `macro_rules!` declaration structurally cannot do is see the type's generics, so a declared generic type restates them, bounds included. The derive reads them off the type:

```rust
#[derive(Default, structio::Structio)]
struct Page<'a, T: Clone, const N: usize>
where
    T: PartialEq,
{
    label: &'a str,
    items: Vec<T>,
}
```

becomes

```rust
structio::object!(['a, T: Clone + PartialEq + ::structio::ReadWrite + ::core::default::Default, const N: usize] Page<'a, T, N> {
    label, items
});
```

Each type parameter gets the format's read-and-write bound and `Default` appended, since the impls read and write through it; `json` and `beve` narrow that to `json::ReadWrite` or `beve::ReadWrite`. A parameter's default is dropped, as an impl requires. A `where` clause is folded onto the parameters it bounds. A predicate on anything else, `Vec<T>: Clone` say, has nowhere to go and is refused at the predicate: the macros take bounds inline and nothing else.

A lifetime is the input lifetime, exactly as it is for a declared type that leads with one.

## Enums

```rust
#[derive(Default, structio::Structio)]
#[structio(rename_all = "snake_case")]
enum Level {
    #[default]
    Info,
    #[structio(rename = "WARN")]
    Warning,
}

#[derive(Default, structio::Structio)]
#[structio(tag = "kind")]
enum Shape {
    #[default]
    Empty,
    Circle(Circle),
}
```

An enum whose variants all carry nothing, with no `tag`, is a `unit_enum!`, so a run of it in BEVE is a string array. Any payload or a `tag` makes it a `tagged_enum!`. A narrowed unit enum, `#[structio(json)]` or `#[structio(beve)]`, is the one-format tagged macro instead, since `unit_enum!` has no one-format form: its JSON half is the same code, and its BEVE half differs only in that string-array packing, which the tagged form reads back all the same.

A variant carries at most one value, and the value is a type of its own. That is the shape the enum macros take, and [why](enums.md#one-payload-of-a-type-you-already-declared). A variant with two values is refused with a message saying to give them a struct or a tuple. A variant with named fields is refused too, for now: it is a stage 2 shape, below. A discriminant, `High = 10`, is a Rust-side number and is ignored.

## Where errors land

An attribute the derive refuses is reported at the attribute: an unknown name, a value where none is taken, a rule that is not a case rule, `rename` on a skipped field, `tag` on a struct, `json` and `beve` together, a name given twice. These are the derive's own messages, and they say what to do instead.

Everything the derive emits carries the span of the token it came from, so an error the macro or the type checker raises about a field, a type with no `Read` impl say, points at that field in the struct rather than at the derive line.

## What it refuses

- **A tuple struct or a unit struct.** The macros find a field by name, and `array!` counts by name too. Give the fields names, or declare a tuple instead.
- **A union.** Which field holds the value is not something the bytes can say.
- **A variant with several values, or with named fields.** See [Enums](#enums).
- **A `where` predicate on anything but the type's own parameters.** See [Generics](#generics).
- **An attribute from a later stage.** Named as such, with the stage, rather than as an unknown attribute.

## Later stages

The derive ships in three stages. This is stage 1, which covers everything the macros can express. Each later stage is a minor release, and an attribute from it on an earlier build is a compile error naming the stage.

**Stage 2** adds the shapes the macros cannot declare, which the derive generates directly through the `ReadObject`, `WriteObject` and `Keys` traits:

- **Named-field variants**, `Window { size: u32, guard: u32 }`, written as `{"kind":"window","size":8,"guard":2}`. Reading goes through a hidden payload struct per variant; writing borrows the fields in place.
- **`tag = "kind", content = "data"`**, adjacent tagging: `{"kind":"NotTracking","data":{"mode":2}}`, with the two members accepted in either order.
- **`alias = "key"`** on a field or variant: one more key accepted on read, pointing at the same field.

**Stage 3** adds per-field policy:

- **`skip_if = "path"`** leaves the member out of the output whenever `path(&field)` is true, under every writer policy. It is the type author's statement about that field and does not interact with `SkipNull`, which stays about `Option`. The alternative, writing `null` under the default policy, would put `null` where a reader expects an array.
- **`default`** on the type generates its `Default` impl, with each field taking `default = "path"` when given and `Default::default()` otherwise. The read model is untouched: absence is still "keeps what the destination held", and the destination now holds the right thing.
- **`transparent`**, a one-field struct written as that field. **`write_only`**, `Write` and no `Read`, for a type that borrows what it writes. **`skip_read`** and **`skip_write`**.

Not planned: `flatten`, which changes the shape of the object the reader sees and would need a member's keys merged into the parent's map, and `deny_unknown_fields`, which is a [read policy](options.md) and stays one.

## Coming from serde

| serde | structio | |
|---|---|---|
| `#[serde(rename_all = "..")]` | `#[structio(rename_all = "..")]` | The rule is not serde's; see [the differences](schemas.md#coming-from-serde). |
| `#[serde(rename = "..")]` | `#[structio(rename = "..")]` | |
| `#[serde(skip)]` | `#[structio(skip)]` | |
| `#[serde(tag = "..")]` | `#[structio(tag = "..")]` | |
| `#[serde(with = "..")]` | `#[structio(with = "..")]` | An adapter type rather than a module of two functions. |
| `#[serde(crate = "..")]` | `#[structio(crate = "..")]` | |
| `#[serde(alias = "..")]` | stage 2 | |
| `#[serde(tag = "..", content = "..")]` | stage 2 | |
| `#[serde(skip_serializing_if = "..")]` | `skip_if`, stage 3 | Omits under every policy. |
| `#[serde(default = "..")]` | `default`, stage 3 | Generates `Default`; the reader is unchanged. |
| `#[serde(transparent)]` | stage 3 | |
| `#[serde(skip_serializing)]` / `skip_deserializing` | `skip_write` / `skip_read`, stage 3 | |
| `#[serde(deny_unknown_fields)]` | none | The default policy already refuses unknown keys; `SkipUnknown` steps over them. A per-type override is not planned. |
| `#[serde(flatten)]` | none | Not planned. |
| `#[serde(untagged)]` | none | A value with no tag has no name to look up. |
| `#[serde(borrow)]` | not needed | A lifetime on the type is the input lifetime. |
| `#[serde(default)]` with no path | `#[derive(Default)]` | Required anyway. |

## Cost

The derive is a second crate, `structio-derive`, built for the host and linked before your code can start. That is the cost, and it is measured in the dependency graph rather than in seconds: the crate has no dependencies of its own and walks `proc_macro::TokenStream` directly, so it builds in well under a second. Expanding a derived type costs what expanding the declaration costs, plus the walk, which [was measured](schema-declaration.md#what-it-came-to) at about two milliseconds per struct.

`structio` pins `structio-derive` to its exact version. The two are published together, the derive first.

## See also

- [Schemas and types](schemas.md) for what each piece of the declaration means once expanded.
- [Enums](enums.md) for the two wire forms and what reading accepts.
- [Schema declaration](schema-declaration.md) for why the macros came first and what the derive prototype measured.
