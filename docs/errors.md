# Errors

Every failure is an `Error`: a one-byte `code` and the `index` where it was detected.

```rust
pub struct Error {
    pub code: ErrorCode,
    pub index: usize,
}
```

One `Error` covers both formats, and it does not record which one it came from. The entry point already said: `from_str` reads JSON and `from_beve` reads BEVE, and there is no call in this crate where the format could have been either. Code that needs to report which codec failed should record it where the codec was chosen, which is the same place it decided what to call. Do not infer it from the code: some are particular to one format (`ExpectedBrace`, `InvalidEscape`, `InvalidHeader`), but most of the set is shared, and which ones are is an implementation detail rather than a promise.

The offset is a byte offset into the input you passed. It is attached once, at the public entry point, from the cursor position at the moment the parse stopped. Inside the hot paths only the bare `ErrorCode` travels, so `Result<(), ErrorCode>` stays register sized and `?` costs a test and a branch.

## Showing an error to a person

`display_with` renders the failure against the input, with a line, a column, and a caret under the offending byte:

```rust
match structio::from_str::<Config>(text) {
    Ok(config) => { /* ... */ }
    Err(e) => eprintln!("{}", e.display_with(text)),
}
```

Long lines are trimmed around the caret so the output stays readable. The input you hand it need not be the document that produced the error; a diagnostic helper that panicked would be worse than useless, so an index that lands inside a multi-byte character is rounded down to a boundary rather than slicing through it.

`Error` also implements `Display` on its own, without the input, and `std::error::Error`.

## Codes

`ErrorCode` is `#[non_exhaustive]`, so match with a `_` arm. `code.message()` gives a short, stable, human-readable description of any of them.

### Structural

| Code | Meaning |
|---|---|
| `UnexpectedEnd` | The input stopped in the middle of a value. |
| `ExpectedBrace` `ExpectedBracket` `ExpectedQuote` `ExpectedColon` `ExpectedComma` | JSON punctuation was required and something else was there. |
| `ExpectedTrue` `ExpectedFalse` `ExpectedNull` | A literal started but did not finish. |
| `UnexpectedCharacter` | A byte appeared where no value could begin. |
| `TrailingContent` | The document ended, and then there was more. |
| `ExceededMaxDepth` | Nesting ran past the limit, which exists so a hostile document cannot exhaust the stack. |
| `DocumentTooLarge` | A streaming reader would have had to buffer more than its configured `max_value`. |

### BEVE structure

| Code | Meaning |
|---|---|
| `InvalidHeader` | A header byte named a type, width, or extension that is not defined. |
| `UnsupportedFeature` | A well-formed construct this crate does not decode: a 128-bit float, an extension beyond the four the specification defines, or, when transcoding, the delimiter or the deprecated type tag. |
| `UnsupportedKeyType` | An object's keys were of a kind the destination cannot take, such as integer keys for a struct. |

### Type mismatches

| Code | Meaning |
|---|---|
| `ExpectedObject` `ExpectedArray` `ExpectedString` `ExpectedBool` `ExpectedInteger` `ExpectedNumber` | The value was well formed but of the wrong kind for the destination. |
| `ExpectedBytes` | An array of one-byte elements was expected, for a borrowed `&[u8]`. |
| `ElementTypeMismatch` | A typed array's stored element type was not the one a bulk read needs. Reading is otherwise lenient about width; the paths that move a whole block at once cannot be. |
| `ExpectedComplex` | A `Complex` was expected but the value was neither a complex extension nor a two-element array. |
| `ExpectedMatrix` | A `Matrix` was expected but the value was neither a matrix extension nor an object holding its three members. |
| `ExpectedVariant` | An enum was neither a variant name nor an object holding exactly one member that names one. |
| `ArrayLengthMismatch` | A fixed-size target (`[T; N]`, a tuple) did not match the input's length. |
| `ExpectedSingleChar` | A `char` was requested but the string was not exactly one scalar value. |
| `UnknownKey` | An object held a key no field of the destination claims. Read with [`SkipUnknown`](options.md#error_on_unknown_keys) to step over it instead. |
| `MissingKey` | An object left out a field the destination declares, under [`RequireKeys`](options.md#error_on_missing_keys). Off by default, absence otherwise meaning the destination keeps what it held. |
| `UnknownVariant` | An enum's tag named no variant the destination declares. Refused under every policy, `SkipUnknown` included: a member with nowhere to go can be stepped over and the object still read, but a variant with nowhere to go leaves the value itself undecided. |
| `InvalidMatrixLayout` | A matrix named a storage order that is not one of the two defined. |
| `InvalidMatrixShape` | A matrix held a different number of elements than its extents describe. Also what `Matrix::new` and `MatrixRef::new` return, which is why `ErrorCode` implements `std::error::Error`: a value assembled in memory has no byte offset to locate it at. |

### Values

| Code | Meaning |
|---|---|
| `NumberOutOfRange` | The number was valid but does not fit the destination type. |
| `InvalidNumber` | The number was not well formed. |
| `InvalidEscape` `InvalidSurrogate` | A JSON string escape was malformed, or a surrogate pair was unpaired. |
| `InvalidUtf8` | The input was not valid UTF-8. |
| `ControlCharacterInString` | An unescaped control character appeared inside a JSON string. |
| `EscapeInBorrowedString` | A borrowed `&str` was requested but the JSON string contained escapes, so no subslice of the input can represent it. Use `Cow<str>`. |

### Pointers

From `from_beve_at` and `read_beve_into_at`. The two are kept apart because only one of them is the document's fault.

| Code | Meaning |
|---|---|
| `InvalidPointer` | The pointer was not valid [RFC 6901](https://www.rfc-editor.org/rfc/rfc6901) syntax: it did not start with `/`, or a token held a stray `~`, or an array index was not a decimal number without leading zeros. |
| `NoSuchValue` | The pointer was well formed but named a member or element the document does not hold. This includes `-`, the position after the last element, which is a valid token that by construction names nothing. |

Whether a pointer is well formed never depends on the document it is aimed at, so the same pointer against two documents gives you the same one of these two answers plus a hit or a miss, never a syntax error against one and a miss against the other.

## Streaming

Streaming adds one thing: I/O can fail as well as the content. `StreamError` separates them.

```rust
pub enum StreamError {
    Io(std::io::Error),
    Parse(Error),
}
```

`as_parse()` and `as_io()` return the respective halves as `Option`, for when you only care about one.

The `Parse` variant carries an ordinary `Error` whose offset is against the **whole stream**, not the current buffer, so it stays meaningful across chunk boundaries.

`From<std::io::Error>` and `From<Error>` are both implemented, so `?` reaches `StreamError` from either side. Nothing converts the other way: an `io::Error` is a boxed allocation and `Error` is a `Copy + Eq` pair of a code and an offset, which is what keeps `Result<(), ErrorCode>` register sized in the parser. Folding I/O into it would charge every parse for a failure mode only a reader has.

See [streaming.md](streaming.md#errors-and-recovery) for which failures are recoverable and which end the stream.

## Writing

`to_writer`, `to_beve_writer` and their variants return `std::io::Result<()>`, not a crate error. Serialization has nothing to fail at: a `Write` implementation returns no error, so the sink is the only thing on that path that can go wrong, and its error comes back unchanged rather than wrapped. There is nothing to match on and nothing to unwrap.

The one write path that returns `StreamError` is `beve_to_json_writer`, because it reads as well: the input can be malformed and the sink can fail, and the two answers are different. Code that wants a single error type across both directions can use `StreamError` there too, since `?` converts an `io::Error` into it.

## What is not an error

Reading is deliberately permissive in two places, both covered in [schemas.md](schemas.md#what-happens-on-the-way-in): an absent key leaves its field untouched, and a repeated key takes the last value. Only the first is now a policy: read with [`RequireKeys`](options.md#error_on_missing_keys) and an absent key is a `MissingKey`. An unknown key used to be a third, and is now an [`UnknownKey`](options.md#error_on_unknown_keys) unless you ask for [`SkipUnknown`](options.md#error_on_unknown_keys).

BEVE reading is additionally lenient about numeric **width**, since a producer writes a number at whatever width its own type had. It is never lenient about **kind**. See [beve.md](beve.md#reading-is-lenient-about-width-strict-about-kind). The one exception is a path that moves a whole numeric block at once rather than an element at a time, where converting is the work being skipped: `read_beve_array_into` says `ElementTypeMismatch` where `from_beve` would widen.
