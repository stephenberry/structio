# Options

Six settings so far: indent the JSON, keep an array on one line while doing it, leave out members that would be null, refuse a key no field claims, refuse a document that leaves a declared field out, and read the comments in one. All are decided at compile time, so the one you do not ask for costs nothing at all.

One trait covers both directions, so a policy names everything a program does with a document rather than making you carry two. A setting belonging to one direction is ignored by the other: `PRETTY` means nothing to a parser and `ERROR_ON_UNKNOWN_KEYS` means nothing to a writer.

## Using one

Every entry point, reading and writing, has a `_with` twin that takes a policy as its first type argument.

```rust
use structio::{Pretty, SkipNull, to_string, to_string_with};

#[derive(Default)]
struct Server { port: u16, tls: Option<String> }
structio::object!(Server { port, tls });

let server = Server { port: 8080, tls: None };

assert_eq!(to_string(&server), r#"{"port":8080,"tls":null}"#);
assert_eq!(to_string_with::<Pretty, _>(&server), "{\n  \"port\": 8080,\n  \"tls\": null\n}");
assert_eq!(to_string_with::<SkipNull, _>(&server), r#"{"port":8080}"#);
```

Reading is the same shape:

```rust
use structio::{ErrorCode, SkipUnknown, from_str, from_str_with};

let doc = r#"{"port":8080,"debug":true}"#;

assert_eq!(from_str::<Server>(doc).unwrap_err().code, ErrorCode::UnknownKey);
assert_eq!(from_str_with::<SkipUnknown, Server>(doc).unwrap().port, 8080);
```

The streaming readers take theirs as one more link in the builder chain they already have:

```rust
let mut docs = Documents::lines(file).with_options::<SkipUnknown>();
```

The plain entry point is the `Standard` policy, so `to_string(v)` and `to_string_with::<Standard, _>(v)` are the same call.

| | |
|---|---|
| `Standard` | Compact, every declared member written, every unknown key refused. What you get without asking. |
| `Pretty` | Indented two spaces per level. |
| `PrettyInlineArrays` | Indented, with each array kept on one line. |
| `SkipNull` | Compact, null members left out. |
| `SkipUnknown` | Unknown keys stepped over rather than refused. |
| `RequireKeys` | Every declared field required to be present. |
| `AllowComments` | JSONC: `//` and `/* */` read wherever whitespace is allowed. |

## Writing your own

The built-ins are single settings, and combinations are deliberately not enumerated. A policy is a unit struct and an impl:

```rust
use structio::Options;

/// Indented four spaces, with absent members left out, and tolerant of a
/// document written against a newer version of the schema.
#[derive(Clone, Copy)]
pub struct Config;

impl Options for Config {
    const PRETTY: bool = true;
    const INDENT: usize = 4;
    const SKIP_NULL: bool = true;
    const ERROR_ON_UNKNOWN_KEYS: bool = false;
}
```

Every constant has a default, so an impl states only what it changes, and a policy written today keeps compiling when a later release adds a setting.

## `PRETTY`

Each member and element goes on its own line, indented by nesting depth, with a space after every colon. A container with nothing in it has nothing to indent and stays on one line as `{}` or `[]`. `NEW_LINES_IN_ARRAYS`, below, is what takes an array's elements off lines of their own.

`INDENT` sets the spaces per level and defaults to 2. It is ignored when `PRETTY` is false.

BEVE ignores both. A binary document has no whitespace to put anywhere, so `to_beve_with::<Pretty, _>` returns the bytes `to_beve` would have.

Transcoding honours it, which is the case worth knowing about. `beve_to_json_with::<Pretty>` is the answer to "what is actually in this file": there is no schema to consult, so the shape has to come off the page.

## `NEW_LINES_IN_ARRAYS`

On by default, which is what indenting a document usually means: every element gets a line. That is the wrong shape for numeric data, where a hundred samples become a hundred lines holding one number each. `PrettyInlineArrays` turns it off, and the setting composes with `INDENT` like any other.

```rust
use structio::{PrettyInlineArrays, to_string_with};

#[derive(Default)]
struct Sample { id: u32, values: Vec<f64> }
structio::object!(Sample { id, values });

let sample = Sample { id: 7, values: vec![1.5, 2.5, 3.5] };

assert_eq!(
    to_string_with::<PrettyInlineArrays, _>(&sample),
    "{\n  \"id\": 7,\n  \"values\": [1.5, 2.5, 3.5]\n}"
);
```

An inline array writes as `[a, b, c]`: no break after the opening bracket or before the closing one, and a space after each comma in their place. It also takes no level of indentation, nothing being indented against it, so an object inside one opens as `[{`, its members sit one level in from the line the array began on, and the next one joins it after the space:

```json
{
  "rows": [{
    "name": "a",
    "count": 1
  }, {
    "name": "b",
    "count": 2
  }],
  "grid": [[1, 2], [3]]
}
```

Objects are untouched, nested arrays go inline too, and an empty array is `[]` either way. The setting is ignored when `PRETTY` is false, where nothing has a line of its own to be kept off, and by BEVE along with the rest of the spacing.

This is Glaze's `new_lines_in_arrays`, down to the name, the `, ` separator, and the level of indentation an inline array does not take, so the same value laid out by both libraries gives the same array. What the two still disagree about is [null members](#glaze-differs-here), which is a different setting.

## Text that is already JSON

A policy decides the layout of a document being *written*, which is no help when the document arrived as text: a response body, a log line, a file on disk. `prettify` lays that out instead, taking the same policy and reaching the same settings.

```rust
use structio::{PrettyInlineArrays, prettify, json::prettify_with};

assert_eq!(prettify(r#"{"a":[1,2]}"#).unwrap(), "{\n  \"a\": [\n    1,\n    2\n  ]\n}");

assert_eq!(
    prettify_with::<PrettyInlineArrays>(r#"{"a":[1,2]}"#).unwrap(),
    "{\n  \"a\": [1, 2]\n}"
);
```

`PRETTY` is honoured rather than assumed, so a compact policy compacts. `prettify_into` writes into a `String` you already have, the way `write_into` does.

The layout is not a second implementation of these settings. The walk emits its whitespace through the same writer the value path uses, so laying out text produces exactly the bytes writing the same data would have, which the tests assert directly and the fuzzer asserts over generated documents. It is also byte-identical to `glz::prettify_json` at the same width, which the benchmark checks.

What the input brought with it is kept: a number holds the spelling the document gave it and a string holds its escapes, so `1.50` stays `1.50` rather than becoming `1.5`. Comments are the exception, since nothing in this crate has a way to write one: `ALLOW_COMMENTS` decides whether a document may carry them, and they are dropped rather than moved.

Structure is checked as the walk goes, because it has to be known before anything can be laid out, so a document whose shape does not hold up is an error at the byte that stopped it. Tokens are not checked past that: a number is taken by its alphabet, so `01` lays out unchanged rather than being refused. This is a formatter, not a validator, and `from_str` is what answers whether a document is good.

`minify` goes the other way, and has no layout to agree with:

```rust
assert_eq!(structio::minify("{\n  \"a\": [1, 2]\n}").unwrap(), r#"{"a":[1,2]}"#);
```

It checks even less, because it needs to. Laying a document out means knowing its shape; taking the layout away means knowing only where the strings are, since whitespace inside one belongs to the document and whitespace outside one belongs to the formatter. So nothing counts brackets and no token is read, which is what makes it the fastest thing here. `ALLOW_COMMENTS` is the only setting it reads, the write settings having nothing to say about a form with no layout.

Three things are still refused, none of them a judgement about the document: a string that never closes, a slash that begins no comment where comments are whitespace, and whitespace that is holding two bare tokens apart, since dropping that would turn `[1 2]` into `[12]` rather than reformatting it.

`prettify_with::<Standard>` also compacts, and agrees with `minify` byte for byte on any document that is really JSON. It walks the structure to get there, so it costs more and rejects more. Reach for it when the answer matters as much as the output.

## `SKIP_NULL`

A member holding nothing is not written at all, key included. That means `None`, `()`, and any wrapper around them, so a `Box<Option<T>>` holding `None` is as absent as a bare `None`.

Absence is the test, not the spelling of the output. A `f64` holding NaN writes as `null`, JSON having no other form for it, but it is a number that is present and it stays. Skipping it would also disagree with BEVE, which stores the NaN itself.

Reading treats an absent member as "leave the destination alone", so a field skipped on the way out comes back as whatever `Default` gave it. A round trip through `Default::default()` returns the `None`.

**Struct members only.** A `None` inside a sequence still writes `null`: dropping it would shorten the sequence and shift every index after it, which changes the data rather than its presentation. A map's entries are also left alone, for two reasons: a null value in a map is data rather than an absent field, and a map's length is not known until it has been walked, which BEVE needs before it writes the first entry.

Both formats honour it. BEVE pays slightly more, because an object states its member count before its members and that count stops being a compile-time constant once members can drop out. See [the member count](#the-beve-member-count).

### Glaze differs here

Glaze's `skip_null_members` defaults to **true**. This crate's default is `Standard`, which writes the null. So the same struct serialized by Glaze and by structio does not produce the same document unless you ask for `SkipNull`. Worth knowing if a C++ producer and a Rust consumer share a schema.

`ERROR_ON_UNKNOWN_KEYS` goes the other way and matches Glaze, so a schema shared between the two agrees on what a stray key means and disagrees on what an absent value looks like. One flip, not two.

## `ERROR_ON_UNKNOWN_KEYS`

A key that no field of the destination claims is an `UnknownKey`, and the error is located at the key so the message names what was not recognized.

This is on by default, which is Glaze's default too, and it is the one setting whose default differs from what this crate did before options existed. The reasoning is that a key nothing claims is far more often a typo, a version skew, or the wrong document entirely than it is something to pass over, and silence is the one response you cannot recover from: the value lands nowhere and nothing says so.

`SkipUnknown` asks for the other behaviour. Reach for it to read a subset of a larger document, or to accept one written by a newer version of a schema that has since grown a field.

**Object keys only.** An `array!` struct has no keys, and an array of the wrong length is already an error. A map claims every key it is given by definition, so nothing in a `HashMap<String, T>` is ever unknown. An enum's tag is not a key either, and turning this off does not make an unrecognized one acceptable: an [`UnknownVariant`](errors.md#codes) stands under every policy, because a variant with nowhere to go leaves the value itself undecided rather than one member of it.

`Matrix` is the one type here whose reader is hand written against a fixed set of keys rather than generated, so it applies the policy itself: a member other than `layout`, `extents` or `value` is unknown like any other. A hand-written `Read` impl of your own that reads keyed data has the same obligation, and nothing enforces it — reading through `read_map` cannot know whether the caller has a fixed key set or arbitrary ones. `position` and `rewind` on the cursor are what let such an impl point the error at the right byte.

A key that the input ended in the middle of is not an unknown key. That is an `UnexpectedEnd`, under either policy: a truncated document is a truncated document, not a schema mismatch.

Both formats honour it, and having it on is cheaper than having it off: refusing costs one branch, where stepping over the value costs a walk proportional to the size of that value, and a value under an unknown key can be arbitrarily large. It is also the reading that looks at strictly fewer bytes, which is what makes it the safer default as well as the faster one.

One consequence worth stating: values under an unknown key are stepped over rather than validated, so `{"junk":1.2.3e--,"real":1}` parses under `SkipUnknown` and is refused under the default. Two different answers for the same bytes, and the strict one is the one that looked at less.

### A pointer is not affected

`from_beve_at` walks to the value its pointer names and reads only that. Nothing on the way is read against a schema, so a key beside the path is not an unknown key to anything. The policy governs the value at the end of the pointer.

## `ERROR_ON_MISSING_KEYS`

A field the destination declares and the document never mentions is a `MissingKey`. The error is located where the object began, its opening brace in JSON and its header byte in BEVE: what is incomplete is the object, not the byte that closed it.

This is **off** by default, where `ERROR_ON_UNKNOWN_KEYS` is on, and the asymmetry is deliberate. Reading is into a value that already exists, so a member the document does not mention means "keep what is there" rather than "no data". That is what makes `read_into` a merge, and a `Default` is a perfectly good answer for a field the document had no opinion about. Turning this on says the opposite, that the document is the whole truth about the value: right for a wire format, wrong for a patch. Glaze has `error_on_missing_keys` and also leaves it off.

`RequireKeys` asks for it. It is the exact opposite of `SkipUnknown`: that one accepts a document saying more than the schema does, this one refuses a document saying less. The two settings are independent. The built-ins are one policy per setting and combinations are yours to declare, which for these two is worth doing:

```rust
use structio::Options;

/// Say at least what the schema says, and say more if you like.
#[derive(Clone, Copy)]
pub struct Superset;

impl Options for Superset {
    const ERROR_ON_UNKNOWN_KEYS: bool = false;
    const ERROR_ON_MISSING_KEYS: bool = true;
}
```

**Object keys only**, for the same reasons the unknown-key setting is: an `array!` struct is judged by its length already, and a map has no declared members to miss. `Matrix` applies the policy by hand as it does the other one, so its object form requires all three of `layout`, `extents` and `value`, reported against the object like any other; the BEVE matrix extension carries all three by construction and never has one to be missing.

An `Option<T>` field is not exempt. The test is whether the member is *present*, not what it holds, so `null` satisfies it and absence does not. Writing under `SKIP_NULL` and reading under `RequireKeys` therefore contradict each other by construction: the writer drops exactly the members the reader insists on.

A repeated key fills one field twice rather than two fields once, so it never stands in for an absent one. An unknown key is refused before an absent one is noticed, the unknown key being refused where it sits and the absent one only once the object has ended.

**At most 64 fields.** The bookkeeping is one bit per field in a single `u64`, set as each field is filled and compared against every field once the object ends. A struct wider than that read under this option is a compile error naming the limit, rather than a wider mask every narrower struct would pay for. The cap belongs to the option and not to the struct: no other setting looks at the field count, and a struct of any width still reads under every other policy.

The refusal is a constant of a generic type, so it is reported when the crate is *built* rather than by `cargo check` or by an editor running one. The message names the limit, and the notes below it name your struct.

It costs one `or` per member the schema claimed and one comparison per object, against a mask that never leaves a register, and nothing at all when it is off. A key nothing claims costs neither.

## `ALLOW_COMMENTS`

`//` to the end of the line and `/* */` anywhere whitespace is allowed, which is JSONC. **Off** by default, a comment being no part of JSON; `AllowComments` asks for it. Glaze reads JSONC too.

**Reading only.** Nothing writes a comment, because nothing holds one: a comment carries no data, so a document read under this and written back out comes back without it. **JSON only**, too. BEVE has no whitespace and so has nowhere to put one, and reading BEVE under this policy is reading it under `Standard`.

A comment goes wherever whitespace goes: before the document, after an opening brace, either side of a colon or a comma, before a closing one, after the last value. It does not go inside a string, where `//` is two ordinary characters and always was. Block comments do not nest, so `/* /* */` is one comment and the rest is document, which is what JSONC, JSON5 and C all say.

A comment is stepped over only when it is **complete**. A `/` that begins nothing, and a `/*` that is never closed, are left exactly where they are, so the error carries the offset of the byte the comment began at and says what was wanted there: a `TrailingContent` at an unclosed comment after the last value, an `ExpectedComma` at one inside an object. Consuming it to the end of the input instead would report `UnexpectedEnd` at a byte nobody wrote, and would quietly accept a trailing one.

The streaming readers honour it too, and have to. They divide a stream into values before the parser sees any of it, and a comment may hold a brace, a bracket, or a quote; a splitter that did not know about one would cut the stream in the wrong place rather than merely pass the comment on. The scan resumes inside a comment as it does inside a string, so a refill landing between the `*` and the `/` that close one costs nothing.

Two things a streamed read does differently follow from its not having all the bytes yet. A comment left open when a stream ends is an `UnexpectedEnd`, where `from_str` reports `TrailingContent` at the opener: one ran out of input and the other had all of it. And a comment's bytes belong to no value's span, so the streaming readers step over them without validating them as text, where `from_slice` checks the whole document as UTF-8 up front and refuses it. Neither can move where the stream is cut, since no byte of a multi-byte sequence is ASCII and a comment therefore cannot hide a newline or a `*/` however it is encoded.

`Mode::Lines` is the exception, and only for block comments. A value's bytes are one line there, which is what makes finding the boundary a search for a single byte, so a comment cannot span lines. A line holding nothing but whitespace and comments carries no value and is skipped exactly as a blank one is, and a comment after the value on a line is the parser's and it takes it. A block comment left open at the end of a line is not joined to the next: both lines are reported and the framing survives, so the values after them still arrive.

It costs one comparison per run of whitespace, against a byte that is already loaded, and nothing at all when it is off.

## The BEVE member count

BEVE writes an object's member count before its members, so the count has to be known first. `WriteObject::count_fields` is where it comes from:

```rust
fn count_fields<O: Options>(&self) -> usize;
```

Without `SKIP_NULL` every field counts, the sum folds to the same literal `KEYS.len()` would have been, and the size prefix is a single store exactly as before. With it, the count depends on the value and the fields are tested.

`object!` generates this alongside `write_fields`, so a declared struct needs nothing. A hand-written `WriteObject` has to supply it, and there is deliberately no default body: a count that disagrees with what `write_fields` writes does not produce a document a reader rejects. It produces one where the reader takes the next value's bytes for a member, or stops short and calls the rest trailing content. That is the failure this format punishes hardest, so the trait asks rather than assumes. An impl that writes every field unconditionally returns `Self::KEYS.len()`.

A debug build checks the two agree and panics naming the type if they do not. The check costs a counter on the member path, so it is not in a release build. It counts members written through `Writer::member`, so unlike the JSON side, a BEVE impl that writes its key bytes by hand will trip the assertion even on a correct document.

## What it costs

Code size, in exchange for speed. The read and write paths are compiled once per policy a program actually uses; one policy costs exactly what no policy parameter cost before it, and the branch that reads a setting folds away before the optimizer sees it. A compact writer emits no indentation code at all and never touches the depth it would have read.

## Why a trait and not a struct

Glaze spells this `glz::read<glz::opts{.prettify = true}>(value, buffer)`: a const parameter of struct type. Rust does not have that on stable.

```
error: `Opts` is forbidden as the type of a const generic parameter
  = note: the only supported types are integers, `bool`, and `char`
```

A `u32` of bit flags would compile, and would work, but it has nowhere to hang documentation and `1 << 7` in a backtrace tells nobody anything. A trait with defaulted associated constants gets the same zero-cost dispatch, names each setting, and keeps existing impls compiling when a setting is added.

## Why the type and not a field

A policy could just as well be read once at construction and stored, `Parser { …, error_on_unknown: bool }`, which would keep every entry point and every policy type exactly as they are and cost a great deal less plumbing: no parameter on `Read::read`, none on the macro-generated impls, none on the stream types.

It is in the type for one reason, and it is not this setting's speed. `ERROR_ON_UNKNOWN_KEYS` is consumed on the path where a key did *not* match, which is already cold, so folding its branch away wins little.

What the type buys is that `fn read<O: Options>(&mut self, p: &mut Parser<'de, O>)` is settled. A trait method signature is the one part of this that a later release cannot change without breaking every hand-written `Read` impl in every downstream crate, and the two settings that motivated it are the ones that sit where the constant matters. `ERROR_ON_MISSING_KEYS` updates a bitmask once per matched field on the hot path, and under the default it folds away to nothing rather than to a load and a test. Comments are a branch inside the whitespace skipper, which every token pays. Neither wants to read a byte off the cursor to find out. Paying the plumbing now, while the only hand-written impls are in this repo, is cheaper than paying it after 0.1.0.

So the seam was built for `ERROR_ON_MISSING_KEYS` and for comments, not for `ERROR_ON_UNKNOWN_KEYS`, which is merely what occasioned writing it. That was a bet on a stated plan rather than a performance result, and both halves have now been collected.

The parameter sits on the *method* rather than on the trait, in both directions:

```rust
pub trait Write {
    fn write<O: Options>(&self, w: &mut Writer<'_, O>);
}

pub trait Read<'de>: Sized {
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()>;
}
```

That is what keeps a bound on a container element spelled `T: Write` rather than `T: Write<O>`. A nested container forwards the writer or parser on and never names `O`.

`Parser<'de, O>` and `Reader<'de, O>` carry the policy so `O` is inferred at every call after the first, and both default to `Standard` where the type is written out. A type parameter default fills in a *type*, though; it does not tell inference what an associated function's `Self` is, so the constructors come in two: `Parser::new` is the `Standard` one and `Parser::with_options` names a policy.

The writers do not have that pair, and keep `Writer::<Standard>::new()`. The asymmetry is deliberate and tracks how the two are used: a reader is hand-driven often enough to be worth the second name, since reaching into a document's bytes is a normal thing to want, while nothing hand-builds a writer when `to_string` and `to_vec` cover it.
