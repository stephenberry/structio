//! The `object!`, `array!` and enum macros.
//!
//! Declarative, not procedural: expansion is a token-tree substitution inside
//! the compiler front end, with no proc-macro crate to build and link first,
//! and no dependency of any kind. It generates exactly the trait impls you
//! would write by hand.
//!
//! The struct macros differ in how a field is found. `object!` writes keys and
//! looks them up in a compile-time perfect hash; `array!` writes nothing but
//! the values and finds a field by counting.
//!
//! `unit_enum!` and `tagged_enum!` declare the other shape a schema can have.
//! A variant's name goes on the wire and comes back through the same perfect
//! hash the keys use, so an enum costs a struct's lookup and nothing more.

/// Declare a struct's schema, for every format.
///
/// Keys default to the field names. Give an explicit key with `"name" =>
/// field` when the encoded name differs from the Rust one.
///
/// ```
/// #[derive(Default)]
/// struct Person {
///     first_name: String,
///     age: u32,
/// }
///
/// structio::object!(Person { first_name, age });
/// ```
///
/// Renaming:
///
/// ```
/// # #[derive(Default)]
/// # struct Person { first_name: String, age: u32 }
/// structio::object!(Person {
///     "first-name" => first_name,
///     age,
/// });
/// ```
///
/// A declaration whose keys differ from the Rust names by a rule rather than
/// one at a time names the rule once, after the type. Every key it does not
/// spell out is then converted during compilation.
///
/// ```
/// # use structio::to_string;
/// #[derive(Default)]
/// struct Camera { field_of_view: f32, near_plane: f32, sensor_id: u32 }
///
/// structio::object!(Camera as "camelCase" {
///     field_of_view,
///     near_plane,
///     "sensorID" => sensor_id,
/// });
///
/// assert_eq!(
///     to_string(&Camera::default()),
///     r#"{"fieldOfView":0,"nearPlane":0,"sensorID":0}"#,
/// );
/// ```
///
/// The rules are `"lowercase"`, `"UPPERCASE"`, `"PascalCase"`, `"camelCase"`,
/// `"snake_case"`, `"SCREAMING_SNAKE_CASE"`, `"kebab-case"` and
/// `"SCREAMING-KEBAB-CASE"`. An explicit key wins over the rule wherever both
/// are present, which is what makes `"sensorID" => sensor_id` above the escape
/// hatch for a name the rule spells differently than the format does.
///
/// [`case`](crate::case) has the rule in full. Two parts of it are worth
/// knowing before reaching for one: a leading or trailing `_` is dropped, so
/// `type_` converts to `type`, and a run of capitals is respelled as one word,
/// so `http_url` under `"camelCase"` is `httpUrl` rather than `httpURL`. The
/// spellings are `serde`'s but the rule is not, so a schema being ported
/// should check [the differences](crate::case#coming-from-serde) first.
///
/// A member a document has to carry is marked `#[required]`. Absence is
/// otherwise no error: an unmarked field the document leaves out keeps
/// whatever the destination already held.
///
/// ```
/// # use structio::{ErrorCode, from_str};
/// #[derive(Debug, Default)]
/// struct Asset { version: String, min_version: u32, generator: String }
///
/// structio::object!(Asset {
///     #[required] version,
///     #[required] "minVersion" => min_version,
///     generator,
/// });
///
/// // The optional member may be left out.
/// assert!(from_str::<Asset>(r#"{"version":"2.0","minVersion":1}"#).is_ok());
/// // A required one may not.
/// assert_eq!(
///     from_str::<Asset>(r#"{"generator":"g"}"#).unwrap_err().code,
///     ErrorCode::MissingKey,
/// );
/// ```
///
/// This is the type's own answer, so it holds under every policy, and
/// [`RequireKeys`](crate::RequireKeys) still requires the members no mark did.
/// It is the setting to reach for where a schema is mixed, which is most of
/// them. See [`Keys::REQUIRED`](crate::Keys::REQUIRED) for the mask it writes
/// and the one limit on it: a marked field must be among the first 64
/// declared.
///
/// A field whose type this crate does not describe, and which you cannot
/// implement the traits for because you own neither of them, names an
/// *adapter* instead: a type of your own that says how that type is read and
/// written. The field keeps its own type.
///
/// ```
/// # use std::time::Duration;
/// # use structio::{ErrorCode, Options, json};
/// // `Millis` reads and writes a `Duration` as a whole number of
/// // milliseconds. Its two impls are in `examples/adapters.rs`; see
/// // `json::ReadAs` for their signatures.
/// # struct Millis;
/// # impl<'de> json::ReadAs<'de, Duration> for Millis {
/// #     fn read<O: Options>(v: &mut Duration, p: &mut json::Parser<'de, O>)
/// #         -> Result<(), ErrorCode>
/// #     {
/// #         let mut ms = 0u64;
/// #         json::Read::read(&mut ms, p)?;
/// #         *v = Duration::from_millis(ms);
/// #         Ok(())
/// #     }
/// # }
/// # impl json::WriteAs<Duration> for Millis {
/// #     fn write<O: Options>(v: &Duration, w: &mut json::Writer<'_, O>) {
/// #         json::Write::write(&(v.as_millis() as u64), w);
/// #     }
/// # }
/// #[derive(Default)]
/// struct Job { id: u32, elapsed: Duration, retries: Vec<Duration> }
///
/// // A `json_object!`, because only the JSON half of `Millis` exists.
/// structio::json_object!(Job {
///     id,
///     "elapsed_ms" => elapsed as Millis,
///     retries as Vec<Millis>,
/// });
///
/// let job = Job { id: 1, elapsed: Duration::from_millis(90), retries: vec![] };
/// assert_eq!(structio::to_string(&job), r#"{"id":1,"elapsed_ms":90,"retries":[]}"#);
/// ```
///
/// Adapters compose as types do, so `Option<Millis>` and `Vec<Millis>` adapt
/// the containers and [`Same`](crate::Same) is the identity. See
/// [`json::ReadAs`](crate::json::ReadAs).
///
/// Generic and borrowing types take their impl generics in brackets. Write
/// `'de` yourself when the type borrows from the input; it is the lifetime of
/// the document being read.
///
/// ```
/// #[derive(Default)]
/// struct Borrowed<'a> {
///     name: &'a str,
/// }
/// structio::object!(['de] Borrowed<'de> { name });
///
/// #[derive(Default)]
/// struct Page<T> {
///     items: Vec<T>,
///     cursor: Option<String>,
/// }
/// structio::object!([T: structio::ReadWrite + Default] Page<T> { items, cursor });
/// ```
///
/// # What it expands to
///
/// One [`Keys`](crate::Keys) impl carrying the key list and the compile-time
/// perfect hash, and then, for each format, four small impls: `ReadObject` and
/// `WriteObject` for the per-field dispatch, and `Read`/`Write` delegating to
/// the object forms.
///
/// The schema is declared once because it *is* one thing: the same field
/// order, the same keys, and the same hash table serve
/// [`json`](crate::json) and [`beve`](crate::beve) alike. Only the bytes
/// differ. Nothing is hidden; the same code written by hand behaves
/// identically.
///
/// An adapter is the one place that can stop being true. One name at a field
/// site stands for four impls, and nothing checks that its JSON half and its
/// BEVE half describe the same value, or that their
/// [`is_null`](crate::json::WriteAs::is_null) answers agree. Keeping them
/// saying the same thing is the adapter author's job, and it is worth stating
/// because everything else here makes it impossible to get wrong.
///
/// For a struct encoded as a positional array rather than as a keyed object,
/// see [`array!`](crate::array).
///
/// When a type cannot support both formats, declare it with
/// [`json_object!`](crate::json_object) or
/// [`beve_object!`](crate::beve_object) instead. Every field's type has to be
/// readable in each format generated for, so a struct holding a borrowed
/// `&[u8]`, which only BEVE can hand back, is a `beve_object!`.
#[macro_export]
macro_rules! object {
    ($($t:tt)*) => { $crate::__declare!(__both_impls $($t)*); };
}

/// Declare a struct's schema for JSON alone.
///
/// The same syntax as [`object!`], generating only the JSON impls. Reach for
/// it when a type cannot support both formats: a field whose type only one of
/// them can read, or a struct you simply do not want the other's code
/// generated for.
#[macro_export]
macro_rules! json_object {
    ($($t:tt)*) => { $crate::__declare!(__json_impls $($t)*); };
}

/// Declare a struct's schema for BEVE alone.
///
/// The counterpart of [`json_object!`]. A struct with a borrowed `&[u8]` field
/// needs this one: JSON has no way to hand back a run of bytes out of a
/// document, so there is no JSON impl to generate.
///
/// ```
/// #[derive(Default)]
/// struct Frame<'a> {
///     id: u32,
///     payload: &'a [u8],
/// }
/// structio::beve_object!(['de] Frame<'de> { id, payload });
/// ```
#[macro_export]
macro_rules! beve_object {
    ($($t:tt)*) => { $crate::__declare!(__beve_impls $($t)*); };
}

/// Normalize a declaration's generics, then hand them to `$m` alongside the
/// shared `Keys` impl.
///
/// The three public macros differ only in which impls they want, and the rule
/// for `'de` is the same for all of them: reading borrows from the input, so
/// the read impls always need the lifetime, while the write impls must not
/// declare one they do not constrain. Stating that once is the point.
///
/// The two lists handed on are the read generics and the write generics, in
/// that order, and behind them the declaration's [case rule](crate::case) or
/// `_` for none.
///
/// Each generics form needs two arms, since `macro_rules!` cannot make `as
/// "camelCase"` optional in front of a `$ty:ty`. They differ only in what they
/// put in the case slot, so all six hand the normalized form to one place.
#[doc(hidden)]
#[macro_export]
macro_rules! __declare {
    // Generics that already declare `'de`: the type borrows from the input, so
    // both impls use the list verbatim.
    ($m:ident [ 'de $($gen:tt)* ] $ty:ty as $case:tt { $($body:tt)* }) => {
        $crate::__declared!($m ['de $($gen)*] ['de $($gen)*] [$case] $ty { $($body)* });
    };
    ($m:ident [ 'de $($gen:tt)* ] $ty:ty { $($body:tt)* }) => {
        $crate::__declared!($m ['de $($gen)*] ['de $($gen)*] [_] $ty { $($body)* });
    };
    // Generics without `'de`: the read impls need it, the write impls must not
    // declare an unconstrained lifetime.
    ($m:ident [ $($gen:tt)* ] $ty:ty as $case:tt { $($body:tt)* }) => {
        $crate::__declared!($m ['de, $($gen)*] [$($gen)*] [$case] $ty { $($body)* });
    };
    ($m:ident [ $($gen:tt)* ] $ty:ty { $($body:tt)* }) => {
        $crate::__declared!($m ['de, $($gen)*] [$($gen)*] [_] $ty { $($body)* });
    };
    ($m:ident $ty:ty as $case:tt { $($body:tt)* }) => {
        $crate::__declared!($m ['de] [] [$case] $ty { $($body)* });
    };
    ($m:ident $ty:ty { $($body:tt)* }) => {
        $crate::__declared!($m ['de] [] [_] $ty { $($body)* });
    };
}

/// A declaration whose generics and case rule are both in normal form.
#[doc(hidden)]
#[macro_export]
macro_rules! __declared {
    ($m:ident [$($rgen:tt)*] [$($wgen:tt)*] [$case:tt] $ty:ty { $($body:tt)* }) => {
        $crate::__case_check!($case);
        $crate::__keys_impl!([$($wgen)*] [$case] $ty { $($body)* });
        $crate::$m!([$($rgen)*] [$($wgen)*] [$case] $ty { $($body)* });
    };
}

/// Both formats, for [`object!`].
#[doc(hidden)]
#[macro_export]
macro_rules! __both_impls {
    ([$($rgen:tt)*] [$($wgen:tt)*] [$case:tt] $ty:ty { $($body:tt)* }) => {
        $crate::__json_impls!([$($rgen)*] [$($wgen)*] [$case] $ty { $($body)* });
        $crate::__beve_impls!([$($rgen)*] [$($wgen)*] [$case] $ty { $($body)* });
    };
}

/// Refuse a declaration that names the same field or variant twice.
///
/// One constant per name, in a scope of its own, so a repeat is `E0428`: "the
/// name `f` is defined multiple times", pointed at the declaration and naming
/// the duplicate.
///
/// It has to be a hard error rather than a lint. Duplicating a `match` arm or a
/// pattern binding would be diagnosed too, but as `unreachable_patterns` and
/// `E0025`, and a lint raised inside a macro expanded from *another* crate is
/// suppressed, so it would never reach the person who wrote the declaration.
/// An explicit `#[deny]` on the arm does not lift that; only an error does.
///
/// The key hash refuses a duplicate *key* already, which catches the same
/// mistake whenever the names are the wire names. This is the half it cannot
/// see: two spellings of one member, `"x" => f` beside `"z" => f`, which reads
/// under either name and writes the member twice. A positional struct has no
/// keys at all, so this is the only thing standing between `[x, y, x]` and an
/// array of three elements, two of them the same field.
#[doc(hidden)]
#[macro_export]
macro_rules! __each_name_once {
    ($($name:ident),* $(,)?) => {
        const _: () = {
            $( #[allow(dead_code, non_upper_case_globals)] const $name: () = (); )*
        };
    };
}

/// Whether a field carried the `#[required]` marker.
///
/// A field's markers reach [`__keys_impl!`](crate::__keys_impl) as an optional
/// token, which `macro_rules!` can act on only by handing it to a macro that
/// branches on its presence. The last arm is what turns a misspelling into a
/// message rather than into "no rules expected this token".
#[doc(hidden)]
#[macro_export]
macro_rules! __is_required {
    () => {
        false
    };
    (required) => {
        true
    };
    ($other:ident) => {
        ::core::compile_error!(::core::concat!(
            "unrecognized field marker `#[",
            ::core::stringify!($other),
            "]`; the only one is `#[required]`"
        ))
    };
}

/// The format-independent half: the key list and its compile-time hash.
#[doc(hidden)]
#[macro_export]
macro_rules! __keys_impl {
    (
        [$($wgen:tt)*] [$case:tt] $ty:ty {
            $($(#[$req:ident])? $($key:literal =>)? $field:ident $(as $with:ty)?),* $(,)?
        }
    ) => {
        $crate::__each_name_once!($($field),*);

        impl<$($wgen)*> $crate::Keys for $ty {
            const KEYS: &'static [&'static str] = &[
                $( $crate::__json_key!([$case] $($key)? [$field]) ),*
            ];
            // Built from `Self::KEYS` rather than a second copy of the key
            // list, so the two cannot drift apart.
            //
            // `&` on a const expression promotes to an anonymous static, so the
            // table lives in read-only memory and is never copied onto the
            // stack at a lookup site.
            const MAP: &'static $crate::KeyMap = &$crate::KeyMap::build(Self::KEYS);

            // `macro_rules!` cannot count, so the field index is carried in a
            // counter, here through const evaluation rather than through the
            // optimizer. Zero for a declaration that marks nothing, which is
            // what takes the check back out of both readers.
            #[allow(unused_assignments, unused_mut)]
            const REQUIRED: u64 = {
                let mut mask = 0u64;
                let mut i = 0u32;
                $(
                    if $crate::__is_required!($($req)?) {
                        assert!(
                            i < 64,
                            "a #[required] field must be one of the first 64 \
                             declared: the mask that tracks them is a u64"
                        );
                        mask |= 1u64 << i;
                    }
                    i += 1;
                )*
                mask
            };
        }
    };
}

/// The `#[required]` marker is matched and dropped here: which members a
/// document has to carry is a property of the schema, so only
/// [`__keys_impl!`](crate::__keys_impl) acts on it.
#[doc(hidden)]
#[macro_export]
macro_rules! __json_impls {
    (
        [$($rgen:tt)*] [$($wgen:tt)*] [$case:tt] $ty:ty {
            $($(#[$req:ident])? $($key:literal =>)? $field:ident $(as $with:ty)?),* $(,)?
        }
    ) => {
        impl<$($rgen)*> $crate::json::ReadObject<'de> for $ty {
            // Deliberately not `inline(always)`: this body holds the parser for
            // every field, so forcing it into each caller duplicates a whole
            // nested struct's parser per field arm of its parent.
            #[inline]
            #[allow(unused_assignments, unused_variables, unused_mut)]
            fn read_field<O: $crate::Options>(
                &mut self,
                index: usize,
                p: &mut $crate::json::Parser<'de, O>,
            ) -> ::core::result::Result<bool, $crate::ErrorCode> {
                // `macro_rules!` cannot count, so the field index is carried in
                // a counter that const-folds away. LLVM rebuilds the same jump
                // table a `match` would have produced, and expansion stays
                // linear in the field count instead of quadratic.
                let mut i = 0usize;
                $(
                    if index == i {
                        // The hash only proposed this field; confirm the key
                        // before touching the value.
                        if !p.match_key($crate::__json_key!([$case] $($key)? [$field])) {
                            return ::core::result::Result::Ok(false);
                        }
                        p.colon()?;
                        $crate::__json_read_as!(&mut self.$field, p $(, $with)?)?;
                        return ::core::result::Result::Ok(true);
                    }
                    i += 1;
                )*
                ::core::result::Result::Ok(false)
            }
        }

        impl<$($wgen)*> $crate::json::WriteObject for $ty {
            #[inline]
            fn write_fields<O: $crate::Options>(
                &self,
                w: &mut $crate::json::Writer<'_, O>,
            ) {
                // The duplicate-key check is in `KeyMap::build`, which nothing
                // reaches but `Keys::MAP`. Reading looks a key up and so
                // evaluates it; writing has no use for it, and a generic
                // type's associated const is evaluated only when something
                // names it, so a generic declaration that is never read would
                // otherwise write two members under one key. Naming it here
                // costs nothing and closes that.
                const {
                    let _ = <Self as $crate::Keys>::MAP;
                };
                // Each member carries its own trailing comma; the caller turns
                // the last one into `}`. No per-field "first?" branch.
                $( $crate::__write_member!(
                    w,
                    $crate::__json_member!([$case] $($key)? [$field]),
                    &self.$field
                    $(, $with)?
                ); )*
            }
        }

        impl<$($rgen)*> $crate::json::Read<'de> for $ty {
            #[inline]
            fn read<O: $crate::Options>(
                &mut self,
                p: &mut $crate::json::Parser<'de, O>,
            ) -> ::core::result::Result<(), $crate::ErrorCode> {
                p.read_object(self)
            }
        }

        impl<$($wgen)*> $crate::json::Write for $ty {
            #[inline]
            fn write<O: $crate::Options>(&self, w: &mut $crate::json::Writer<'_, O>) {
                w.write_object(self);
            }
        }
    };
}

/// The `#[required]` marker is matched and dropped here: which members a
/// document has to carry is a property of the schema, so only
/// [`__keys_impl!`](crate::__keys_impl) acts on it.
#[doc(hidden)]
#[macro_export]
macro_rules! __beve_impls {
    (
        [$($rgen:tt)*] [$($wgen:tt)*] [$case:tt] $ty:ty {
            $($(#[$req:ident])? $($key:literal =>)? $field:ident $(as $with:ty)?),* $(,)?
        }
    ) => {
        impl<$($rgen)*> $crate::beve::ReadObject<'de> for $ty {
            #[inline]
            #[allow(unused_assignments, unused_variables, unused_mut)]
            fn read_field<O: $crate::Options>(
                &mut self,
                index: usize,
                key: &[u8],
                r: &mut $crate::beve::Reader<'de, O>,
            ) -> ::core::result::Result<bool, $crate::ErrorCode> {
                let mut i = 0usize;
                $(
                    if index == i {
                        // The key arrived already delimited by its length
                        // prefix, so confirming the hash's candidate is one
                        // slice comparison against a constant.
                        if key != $crate::__json_key!([$case] $($key)? [$field]).as_bytes() {
                            return ::core::result::Result::Ok(false);
                        }
                        $crate::__beve_read_as!(&mut self.$field, r $(, $with)?)?;
                        return ::core::result::Result::Ok(true);
                    }
                    i += 1;
                )*
                ::core::result::Result::Ok(false)
            }
        }

        impl<$($wgen)*> $crate::beve::WriteObject for $ty {
            #[inline]
            fn write_fields<O: $crate::Options>(
                &self,
                w: &mut $crate::beve::Writer<'_, O>,
            ) {
                // The duplicate-key check is in `KeyMap::build`, which nothing
                // reaches but `Keys::MAP`. Reading looks a key up and so
                // evaluates it; writing has no use for it, and a generic
                // type's associated const is evaluated only when something
                // names it, so a generic declaration that is never read would
                // otherwise write two members under one key. Naming it here
                // costs nothing and closes that.
                const {
                    let _ = <Self as $crate::Keys>::MAP;
                };
                // The member count went out with the object header, so a
                // member is its pre-encoded key followed by its value and
                // nothing else.
                $( $crate::__write_member!(
                    w,
                    $crate::__beve_key_bytes!($crate::__json_key!([$case] $($key)? [$field])),
                    &self.$field
                    $(, $with)?
                ); )*
            }

            #[inline]
            #[allow(unused_mut)]
            fn count_fields<O: $crate::Options>(&self) -> usize {
                // Without `SKIP_NULL` every term is `1` and the whole sum
                // folds to the same literal `KEYS.len()` would have been.
                let mut n = 0usize;
                $(
                    n += !(O::SKIP_NULL
                        && $crate::__beve_is_null_as!(&self.$field $(, $with)?)) as usize;
                )*
                n
            }
        }

        impl<$($rgen)*> $crate::beve::Read<'de> for $ty {
            #[inline]
            fn read<O: $crate::Options>(
                &mut self,
                r: &mut $crate::beve::Reader<'de, O>,
            ) -> ::core::result::Result<(), $crate::ErrorCode> {
                r.read_object(self)
            }
        }

        impl<$($wgen)*> $crate::beve::Write for $ty {
            #[inline]
            fn write<O: $crate::Options>(&self, w: &mut $crate::beve::Writer<'_, O>) {
                w.write_object(self);
            }
        }
    };
}

/// The key for a field: the explicit literal, the field name, or the field
/// name put through the declaration's [case rule](crate::case).
///
/// The case slot holds `_` when the declaration named no rule. An explicit
/// literal wins over a rule wherever both are present, which is what makes
/// `"httpURL" => http_url` the escape hatch for a name the rule spells
/// differently than you would.
#[doc(hidden)]
#[macro_export]
macro_rules! __json_key {
    ([$case:tt] $key:literal [$field:ident]) => {
        $key
    };
    ([_] [$field:ident]) => {
        ::core::stringify!($field)
    };
    ([$case:tt] [$field:ident]) => {
        $crate::__case_apply!($case, ::core::stringify!($field))
    };
}

/// The pre-quoted `"key":` prefix, assembled during const evaluation so that
/// writing a JSON member is one copy of a constant string.
///
/// A function of [`__json_key!`](crate::__json_key) rather than a second copy
/// of the literal-or-name-or-rule choice, so the prefix a member is written
/// with and the key the reader confirms cannot come to describe different
/// members. The BEVE side is built the same way, from the same call.
///
/// `concat!` would do for the two forms whose key *is* a literal, and did
/// before there was a third: a converted key exists only once a `const fn` has
/// run, and `concat!` takes literals and nothing else.
#[doc(hidden)]
#[macro_export]
macro_rules! __json_member {
    ($($key:tt)*) => {{
        const KEY: &str = $crate::__json_key!($($key)*);
        const N: usize = KEY.len() + 3;
        // Braced for `__beve_key_bytes!`'s reason: a bare `N` in
        // generic-argument position parses as a type.
        const PREFIX: [u8; N] = $crate::json::quoted_key::<{ N }>(KEY);
        const OUT: &str = $crate::case::as_str(&PREFIX);
        OUT
    }};
}

/// The pre-encoded `SIZE | KEY` bytes of a BEVE object key, assembled during
/// const evaluation so writing a member is one copy of a constant array.
///
/// Unlike [`__json_member!`](crate::__json_member), which needs its own copy of
/// the literal-or-field-name choice because `concat!` cannot take a macro call,
/// this binds its argument to a `const` first and so accepts one directly.
#[doc(hidden)]
#[macro_export]
macro_rules! __beve_key_bytes {
    ($key:expr) => {{
        const KEY: &str = $key;
        const N: usize = $crate::beve::header::key_len(KEY);
        // Braced, because a bare `N` in generic-argument position parses as a
        // type: a user struct called `N` would win over this constant and the
        // declaration would not compile. `macro_rules!` hygiene does not cover
        // items, so the block's own `N` is no protection.
        const ENCODED: [u8; N] = $crate::beve::header::encode_key::<{ N }>(KEY);
        &ENCODED
    }};
}

/// A name put through a case rule, during const evaluation.
///
/// The result is a `const` item, so it lives in read-only memory and the
/// declaration pays for the conversion once at compile time. See
/// [`case`](crate::case) for what the rule does to a name.
///
/// The rule reaches [`case::style`](crate::case::style) as an expression
/// rather than being matched against a list of spellings here, for the reason
/// the adapter helpers below give: a fragment captured by someone else's macro
/// does not re-match a token, so a wrapper macro passing its own
/// `$rule:literal` along would find every spelling rejected.
#[doc(hidden)]
#[macro_export]
macro_rules! __case_apply {
    ($case:literal, $name:expr) => {{
        const NAME: &str = $name;
        // Bound before it is looked up so that a literal of the wrong kind is
        // "expected `&str`" at the declaration rather than a type error deep
        // inside an expansion.
        const RULE: &str = $case;
        const STYLE: $crate::case::Style = $crate::case::style(RULE);
        const N: usize = $crate::case::cased_len(NAME, STYLE);
        // Braced for `__beve_key_bytes!`'s reason.
        const CASED: [u8; N] = $crate::case::cased::<{ N }>(NAME, STYLE);
        const OUT: &str = $crate::case::as_str(&CASED);
        OUT
    }};
    // Reported by `__case_check!`, once for the declaration rather than once
    // per site. Falling back to the name keeps the expansion type-correct so
    // that the one error is the one the reader sees.
    ($other:tt, $name:expr) => {
        $name
    };
}

/// Refuse a case rule that is not a string.
///
/// Checked once for the declaration, because [`__case_apply!`] runs at five
/// sites per field and a rule written without its quotes would otherwise be
/// reported five times over.
///
/// [`__case_apply!`]: crate::__case_apply
#[doc(hidden)]
#[macro_export]
macro_rules! __case_check {
    (_) => {};
    ($case:literal) => {};
    ($other:tt) => {
        ::core::compile_error!(::core::concat!(
            "structio: `",
            ::core::stringify!($other),
            "` is not a case rule; a rule is written as a string, as in \
             `object!(Root as \"camelCase\" { .. })`"
        ));
    };
}

// ---------------------------------------------------------------------------
// Adapter dispatch
// ---------------------------------------------------------------------------
//
// A field may name an adapter, and `macro_rules!` cannot branch inside a
// repetition, so each site that cares dispatches to a helper whose two arms are
// "with an adapter" and "without". The optional fragment goes last at every
// call site, so an absent one expands to nothing and selects the second arm.
//
// A captured `ty` re-matches a `ty` matcher in the callee, so passing one on
// like this does not hit the opaque-fragment rule that would bite a `ty`
// re-examined as anything else.

/// Read a field, through its adapter if it named one.
///
/// The `'_` binds to the generated impl's `'de` and the `_` to the field's own
/// type, so an adapter over a borrowing field or over the declaration's own
/// type parameter needs nothing spelled out.
#[doc(hidden)]
#[macro_export]
macro_rules! __json_read_as {
    ($place:expr, $p:ident, $with:ty) => {
        <$with as $crate::json::ReadAs<'_, _>>::read($place, $p)
    };
    ($place:expr, $p:ident) => {
        $crate::json::Read::read($place, $p)
    };
}

/// Write a member, through its adapter if the field named one.
///
/// The one adapter helper the two formats share, for
/// [`__write_variant!`](crate::__write_variant)'s reason: the two calls are
/// spelled the same in both, and only the pre-encoded key differs, so it is
/// passed in rather than built here.
///
/// The adapter appears in no argument of
/// [`member_with`](crate::json::Writer::member_with), so it is turned up
/// explicitly and the `<A, T>` parameter order there is load bearing.
#[doc(hidden)]
#[macro_export]
macro_rules! __write_member {
    ($w:ident, $key:expr, $place:expr, $with:ty) => {
        $w.member_with::<$with, _>($key, $place)
    };
    ($w:ident, $key:expr, $place:expr) => {
        $w.member($key, $place)
    };
}

/// [`__json_read_as!`](crate::__json_read_as) for BEVE.
#[doc(hidden)]
#[macro_export]
macro_rules! __beve_read_as {
    ($place:expr, $r:ident, $with:ty) => {
        <$with as $crate::beve::ReadAs<'_, _>>::read($place, $r)
    };
    ($place:expr, $r:ident) => {
        $crate::beve::Read::read($place, $r)
    };
}

/// Ask whether a member is absent, of its adapter if the field named one.
///
/// Not merely a matter of getting the same answer as the writer: this is the
/// one site that would otherwise put a [`beve::Write`](crate::beve::Write)
/// bound on the very type the adapter exists to avoid describing, so without
/// it an adapted declaration does not compile at all.
#[doc(hidden)]
#[macro_export]
macro_rules! __beve_is_null_as {
    ($place:expr, $with:ty) => {
        <$with as $crate::beve::WriteAs<_>>::is_null($place)
    };
    ($place:expr) => {
        $crate::beve::Write::is_null($place)
    };
}

/// Declare a struct's schema as a positional array, for every format.
///
/// The bracket counterpart of [`object!`]. Fields are encoded in declaration
/// order with no keys at all: JSON writes them between `[` and `]`, BEVE
/// behind a generic-array header.
///
/// ```
/// #[derive(Default, PartialEq, Debug)]
/// struct Vec3 {
///     x: f64,
///     y: f64,
///     z: f64,
/// }
///
/// structio::array!(Vec3 [x, y, z]);
///
/// let v = Vec3 { x: 1.0, y: 2.0, z: 3.0 };
/// assert_eq!(structio::to_string(&v), "[1,2,3]");
/// assert_eq!(structio::from_str::<Vec3>("[1,2,3]").unwrap(), v);
/// ```
///
/// The list holds field names and nothing else. [`object!`]'s `#[required]`
/// marker has no counterpart here and is not accepted: an element is required
/// by its position, and an array of the wrong length is refused under every
/// policy.
///
/// Generics work as they do for [`object!`]: in brackets before the type, with
/// `'de` written out when the type borrows from the input.
///
/// ```
/// #[derive(Default)]
/// struct Labelled<'a, T> {
///     label: &'a str,
///     value: T,
/// }
/// structio::array!(['de, T: structio::ReadWrite + Default] Labelled<'de, T> [label, value]);
/// ```
///
/// # Homogeneous structs
///
/// When every field is the same type, name it in front of the field list, the
/// way an array type names its element:
///
/// ```
/// #[derive(Default, PartialEq, Debug)]
/// struct Rgb {
///     r: u8,
///     g: u8,
///     b: u8,
/// }
///
/// structio::array!(Rgb [u8; r, g, b]);
/// ```
///
/// JSON is unchanged by this. What it buys is BEVE, which stores a run of one
/// type as a **typed array**: one header for the whole run instead of one per
/// element, and the values as a contiguous block. `Rgb` goes out in five bytes
/// rather than eight, three `f64`s in twenty-six rather than twenty-nine, and
/// three `bool`s in three rather than five, since booleans pack one per bit.
/// The bytes are exactly what a `[u8; 3]` of the same values would have
/// produced, which is also what another implementation writes for its own
/// three-component colour.
///
/// The element type is checked: every field has to be it, and a mismatch is a
/// compile error at the declaration. It also has to be [`Copy`], because the
/// payload is one contiguous run and a struct's fields are not required to be
/// laid out as one, so they are gathered into a block first. That bound holds
/// whether or not the type turns out to have a typed array: one that does not,
/// such as another struct, falls back to a generic array and is written
/// exactly as it would have been without the element type.
///
/// Reading is unchanged either way: an array-declared struct takes a generic
/// array or a typed one whatever it was declared as, so adding an element type
/// changes what you write without narrowing what you accept.
///
/// # When to reach for it
///
/// Position is cheaper than a key in every respect: nothing is hashed, nothing
/// is compared, no [`KeyMap`](crate::KeyMap) is built or stored, and the keys
/// themselves are off the wire. For a type whose field names carry no
/// information anyway, a coordinate or a colour or a row of a table, that is
/// most of the per-value cost gone.
///
/// What it costs is room to move. An object can tolerate a field appearing or
/// disappearing, since a reader matches on names and can be asked to step over
/// a key it does not know
/// ([`SkipUnknown`](crate::SkipUnknown); the default refuses it). An array
/// cannot, under any policy: adding, removing, or reordering a field silently
/// changes what every position means, and lengthens or shortens the array,
/// which readers of the old shape reject. Declare a type this way when its
/// shape is fixed by something outside your control, and `object!` otherwise.
///
/// # What it expands to
///
/// One [`Elements`](crate::Elements) impl carrying the field count, and then,
/// for each format, four small impls: `ReadArray` and `WriteArray` for the
/// per-element dispatch, and `Read`/`Write` delegating to the array forms.
/// There is no key list and no hash, because there is nothing to look up.
/// An element type adds the typed-array header and payload writer to BEVE's
/// `WriteArray`, both of which fold away when it is absent.
///
/// A tuple is the same encoding without the names, and goes through the same
/// drivers, so `(f64, f64, f64)` and the `Vec3` above produce identical bytes
/// in both formats.
#[macro_export]
macro_rules! array {
    ($($t:tt)*) => { $crate::__declare_array!(__both_array_impls $($t)*); };
}

/// Declare a struct's schema as a positional array, for JSON alone.
///
/// The same syntax as [`array!`], generating only the JSON impls. The reasons
/// to want it are [`json_object!`]'s.
#[macro_export]
macro_rules! json_array {
    ($($t:tt)*) => { $crate::__declare_array!(__json_array_impls $($t)*); };
}

/// Declare a struct's schema as a positional array, for BEVE alone.
///
/// The counterpart of [`json_array!`], and what a struct with a borrowed
/// `&[u8]` element needs.
#[macro_export]
macro_rules! beve_array {
    ($($t:tt)*) => { $crate::__declare_array!(__beve_array_impls $($t)*); };
}

/// [`__declare!`](crate::__declare) for the array forms.
///
/// The same normalization of `'de`, and the same two lists handed on, but no
/// `Keys` impl: a positional struct has no keys. What it shares instead is its
/// length, which is [`Elements`](crate::Elements), and that is small enough to
/// be generated alongside each set of impls rather than on its own.
#[doc(hidden)]
#[macro_export]
macro_rules! __declare_array {
    // Generics that already declare `'de`: the type borrows from the input, so
    // both impls use the list verbatim.
    //
    // A case rule goes in front of each shape rather than after all three. An
    // arm that fails to match falls through, but a `$ty:ty` handed something
    // that cannot be a type at all is a parse error that ends the expansion,
    // so a trailing arm would never be reached from a generic declaration.
    ($m:ident [ 'de $($gen:tt)* ] $ty:ty as $case:tt [ $($body:tt)* ]) => {
        $crate::__no_array_case!();
    };
    ($m:ident [ 'de $($gen:tt)* ] $ty:ty [ $($body:tt)* ]) => {
        $crate::__elements_impl!(['de $($gen)*] $ty [ $($body)* ]);
        $crate::$m!(['de $($gen)*] ['de $($gen)*] $ty [ $($body)* ]);
    };
    // Generics without `'de`: the read impls need it, the write impls must not
    // declare an unconstrained lifetime.
    ($m:ident [ $($gen:tt)* ] $ty:ty as $case:tt [ $($body:tt)* ]) => {
        $crate::__no_array_case!();
    };
    ($m:ident [ $($gen:tt)* ] $ty:ty [ $($body:tt)* ]) => {
        $crate::__elements_impl!([$($gen)*] $ty [ $($body)* ]);
        $crate::$m!(['de, $($gen)*] [$($gen)*] $ty [ $($body)* ]);
    };
    ($m:ident $ty:ty as $case:tt [ $($body:tt)* ]) => {
        $crate::__no_array_case!();
    };
    ($m:ident $ty:ty [ $($body:tt)* ]) => {
        $crate::__elements_impl!([] $ty [ $($body)* ]);
        $crate::$m!(['de] [] $ty [ $($body)* ]);
    };
}

/// Refuse a [case rule](crate::case) on a positional struct.
///
/// [`array!`] writes no keys at all, so a rule would have nothing to convert
/// and silently doing nothing is the wrong answer. Without this arm the
/// declaration simply fails to match and the error points into this crate
/// rather than at the `as "camelCase"` that caused it.
#[doc(hidden)]
#[macro_export]
macro_rules! __no_array_case {
    () => {
        ::core::compile_error!(
            "structio: a positional struct has no keys, so a case rule has nothing to \
             convert. Case rules belong on `object!`, `unit_enum!` and `tagged_enum!`."
        );
    };
}

/// Both formats, for [`array!`].
#[doc(hidden)]
#[macro_export]
macro_rules! __both_array_impls {
    ([$($rgen:tt)*] [$($wgen:tt)*] $ty:ty [ $($body:tt)* ]) => {
        $crate::__json_array_impls!([$($rgen)*] [$($wgen)*] $ty [ $($body)* ]);
        $crate::__beve_array_impls!([$($rgen)*] [$($wgen)*] $ty [ $($body)* ]);
    };
}

/// The format-independent half: how many elements the array has.
#[doc(hidden)]
#[macro_export]
macro_rules! __elements_impl {
    // With an element type: the fields have to be it, and saying so is the
    // whole difference, so it is checked here rather than left to whichever
    // format happens to use it.
    ([$($wgen:tt)*] $ty:ty [ $elem:ty ; $($field:ident),* $(,)? ]) => {
        $crate::__elements_impl!([$($wgen)*] $ty [ $($field),* ]);

        const _: () = {
            #[allow(dead_code)]
            fn every_field_is_the_element_type<$($wgen)*>(v: &$ty) {
                $( let _: &$elem = &v.$field; )*
            }
        };
    };
    ([$($wgen:tt)*] $ty:ty [ $($field:ident),* $(,)? ]) => {
        $crate::__each_name_once!($($field),*);

        impl<$($wgen)*> $crate::Elements for $ty {
            // Counted by building a slice of the field names and taking its
            // length, since `macro_rules!` cannot count. Both fold away: the
            // slice is never built.
            const LEN: usize =
                <[&'static str]>::len(&[$( ::core::stringify!($field) ),*]);
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __json_array_impls {
    // JSON has one array syntax, so the element type changes nothing here.
    ([$($rgen:tt)*] [$($wgen:tt)*] $ty:ty [ $elem:ty ; $($field:ident),* $(,)? ]) => {
        $crate::__json_array_impls!([$($rgen)*] [$($wgen)*] $ty [ $($field),* ]);
    };
    ([$($rgen:tt)*] [$($wgen:tt)*] $ty:ty [ $($field:ident),* $(,)? ]) => {
        impl<$($rgen)*> $crate::json::ReadArray<'de> for $ty {
            // Deliberately not `inline(always)`, for `read_field`'s reason:
            // this body holds the parser for every element.
            #[inline]
            #[allow(unused_assignments, unused_variables, unused_mut)]
            fn read_element<O: $crate::Options>(
                &mut self,
                index: usize,
                p: &mut $crate::json::Parser<'de, O>,
            ) -> ::core::result::Result<(), $crate::ErrorCode> {
                // The same const-folding counter `read_field` uses, and for
                // the same reason: expansion stays linear in the field count.
                let mut i = 0usize;
                $(
                    if index == i {
                        return $crate::json::Read::read(&mut self.$field, p);
                    }
                    i += 1;
                )*
                // Only reachable from an array longer than the struct, which
                // the driver would have rejected on the count anyway. Failing
                // here stops the parse at the first surplus element instead of
                // reading the rest of a document that cannot fit.
                ::core::result::Result::Err($crate::ErrorCode::ArrayLengthMismatch)
            }
        }

        impl<$($wgen)*> $crate::json::WriteArray for $ty {
            #[inline]
            fn write_elements<O: $crate::Options>(
                &self,
                w: &mut $crate::json::Writer<'_, O>,
            ) {
                // Each element carries its own trailing comma; the caller
                // turns the last one into `]`.
                $( w.element(&self.$field); )*
            }
        }

        impl<$($rgen)*> $crate::json::Read<'de> for $ty {
            #[inline]
            fn read<O: $crate::Options>(
                &mut self,
                p: &mut $crate::json::Parser<'de, O>,
            ) -> ::core::result::Result<(), $crate::ErrorCode> {
                p.read_array(self)
            }
        }

        impl<$($wgen)*> $crate::json::Write for $ty {
            #[inline]
            fn write<O: $crate::Options>(&self, w: &mut $crate::json::Writer<'_, O>) {
                w.write_array(self);
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __beve_array_impls {
    // With an element type, the struct is stored the way a run of that type is
    // stored: one header for the lot, then the elements' payloads back to
    // back. Reading is unchanged, since the array driver takes either form.
    ([$($rgen:tt)*] [$($wgen:tt)*] $ty:ty [ $elem:ty ; $($field:ident),* $(,)? ]) => {
        $crate::__beve_array_body!([$($rgen)*] [$($wgen)*] $ty [ $($field),* ] {
            const ARRAY: ::core::option::Option<&'static [u8]> =
                <$elem as $crate::beve::Write>::ARRAY;

            // The fields are gathered into a block first, because a payload is
            // one contiguous run and a struct's fields are not required to be
            // laid out as one. It costs a copy of the values, which for the
            // types that have a typed array is a few registers, and it is what
            // lets the element type decide the encoding: a run of booleans
            // packs to bits here exactly as it does in a `Vec<bool>`.
            #[inline]
            fn write_payload<O: $crate::Options>(
                &self,
                w: &mut $crate::beve::Writer<'_, O>,
            ) {
                <$elem as $crate::beve::Write>::write_payload(&[$( self.$field ),*], w);
            }
        });
    };
    ([$($rgen:tt)*] [$($wgen:tt)*] $ty:ty [ $($field:ident),* $(,)? ]) => {
        $crate::__beve_array_body!([$($rgen)*] [$($wgen)*] $ty [ $($field),* ] {});
    };
}

/// The BEVE impls themselves, with `$typed` holding the two items that make a
/// struct a typed array, or nothing.
#[doc(hidden)]
#[macro_export]
macro_rules! __beve_array_body {
    (
        [$($rgen:tt)*] [$($wgen:tt)*] $ty:ty [ $($field:ident),* ] { $($typed:tt)* }
    ) => {
        impl<$($rgen)*> $crate::beve::ReadArray<'de> for $ty {
            #[inline]
            #[allow(unused_assignments, unused_variables, unused_mut)]
            fn read_element<O: $crate::Options>(
                &mut self,
                index: usize,
                r: &mut $crate::beve::Reader<'de, O>,
            ) -> ::core::result::Result<(), $crate::ErrorCode> {
                let mut i = 0usize;
                $(
                    if index == i {
                        return $crate::beve::Read::read(&mut self.$field, r);
                    }
                    i += 1;
                )*
                ::core::result::Result::Err($crate::ErrorCode::ArrayLengthMismatch)
            }
        }

        impl<$($wgen)*> $crate::beve::WriteArray for $ty {
            #[inline]
            fn write_elements<O: $crate::Options>(
                &self,
                w: &mut $crate::beve::Writer<'_, O>,
            ) {
                // The element count went out with the array header, so an
                // element is its value and nothing else.
                $( w.element(&self.$field); )*
            }

            $($typed)*
        }

        impl<$($rgen)*> $crate::beve::Read<'de> for $ty {
            #[inline]
            fn read<O: $crate::Options>(
                &mut self,
                r: &mut $crate::beve::Reader<'de, O>,
            ) -> ::core::result::Result<(), $crate::ErrorCode> {
                r.read_array(self)
            }
        }

        impl<$($wgen)*> $crate::beve::Write for $ty {
            #[inline]
            fn write<O: $crate::Options>(&self, w: &mut $crate::beve::Writer<'_, O>) {
                w.write_array(self);
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Declare an enum whose variants carry nothing, for every format.
///
/// The value on the wire is the variant's name, as a string. Names default to
/// the Rust ones; give an explicit name with `"name" => Variant` when the
/// encoded spelling differs.
///
/// ```
/// #[derive(Default, PartialEq, Debug)]
/// enum Level {
///     #[default]
///     Info,
///     Warning,
///     Error,
/// }
///
/// structio::unit_enum!(Level { Info, Warning, Error });
///
/// assert_eq!(structio::to_string(&Level::Warning), "\"Warning\"");
/// assert_eq!(structio::from_str::<Level>("\"Error\"").unwrap(), Level::Error);
/// ```
///
/// Renaming, which is how a Rust name that is not the wire name is handled:
///
/// ```
/// # #[derive(Default, PartialEq, Debug)]
/// # enum Level { #[default] Info, Warning }
/// structio::unit_enum!(Level {
///     "info" => Info,
///     "warning" => Warning,
/// });
/// # assert_eq!(structio::to_string(&Level::Info), "\"info\"");
/// ```
///
/// A [case rule](crate::case) renames the lot at once, and reads a variant
/// name as words rather than as a snake_case string, so the capitals a Rust
/// variant is spelled with are where it splits.
///
/// ```
/// # #[derive(Default, PartialEq, Debug)]
/// # enum Mode { #[default] ReadOnly, ReadWrite }
/// structio::unit_enum!(Mode as "kebab-case" { ReadOnly, ReadWrite });
/// # assert_eq!(structio::to_string(&Mode::ReadWrite), "\"read-write\"");
/// ```
///
/// # Why it is a macro of its own
///
/// It will not compile if a variant carries a value, so the wire form is a
/// plain string and stays one. That promise is worth stating on its own for a
/// type whose encoding other people depend on, and in BEVE it pays for itself:
/// a value that can only be a string means a run of them is a **string array**,
/// one header for the whole run, so a `Vec<Level>` comes out byte for byte what
/// a `Vec<String>` of the same names would.
/// [`tagged_enum!`](crate::tagged_enum) cannot do that even for a declaration
/// that happens to be all unit variants, since a variant carrying a value
/// writes an object. Reading is unaffected either way: a sequence of enums
/// takes a string array or a generic one however it was written.
///
/// # What it expands to
///
/// One [`Variants`](crate::Variants) impl carrying the name list and its
/// compile-time perfect hash, and then, for each format, `ReadEnum` and
/// `Read`/`Write`, with BEVE's `Write` also carrying the string array. The
/// names are hashed by the same [`KeyMap`](crate::KeyMap) that finds a
/// struct's fields.
///
/// # Further reading
///
/// [docs/enums.md](https://github.com/stephenberry/structio/blob/main/docs/enums.md) is the long form: every error an enum can
/// produce and what distinguishes it from the others, how the policies meet a
/// tag, generics and borrowed payloads, the string array a run of unit
/// variants becomes in BEVE, and how validation, pointers and transcoding walk
/// through one.
#[macro_export]
macro_rules! unit_enum {
    // Every variant a bare name. Parentheses do not parse here, and that
    // refusal is what lets the impls below know the value is always a string.
    //
    // Generics take a rule of their own rather than an optional group, because
    // a type may itself begin with `[` and the parser cannot tell which is
    // meant until it has committed.
    ([$($gen:tt)*] $ty:ty as $case:tt { $($($name:literal =>)? $variant:ident),* $(,)? }) => {
        $crate::__declare_enum!(
            __both_unit_enum_impls [$($gen)*] $ty as $case { $($($name =>)? $variant),* }
        );
    };
    ([$($gen:tt)*] $ty:ty { $($($name:literal =>)? $variant:ident),* $(,)? }) => {
        $crate::__declare_enum!(
            __both_unit_enum_impls [$($gen)*] $ty { $($($name =>)? $variant),* }
        );
    };
    ($ty:ty as $case:tt { $($($name:literal =>)? $variant:ident),* $(,)? }) => {
        $crate::__declare_enum!(
            __both_unit_enum_impls $ty as $case { $($($name =>)? $variant),* }
        );
    };
    ($ty:ty { $($($name:literal =>)? $variant:ident),* $(,)? }) => {
        $crate::__declare_enum!(
            __both_unit_enum_impls $ty { $($($name =>)? $variant),* }
        );
    };
    // Anything else, which is overwhelmingly a variant written `Name(_)`.
    // Without this the failure is `no rules expected `(`` pointed at a matcher
    // inside this crate, which tells the reader nothing about what to do.
    ($($rest:tt)*) => {
        ::core::compile_error!(
            "`unit_enum!` takes a type and a brace-delimited list of variant \
             names, each optionally renamed with `\"name\" => Variant`. A \
             variant that carries a value, written `Variant(_)`, belongs to \
             `tagged_enum!` instead."
        );
    };
}

/// Declare an enum, for every format.
///
/// A variant that carries nothing is written as its name. A variant that
/// carries a value is written as an object of one member, keyed by that name:
/// the tagged-union form, with the tag being the name rather than a position,
/// so adding or reordering variants does not change what a document means.
///
/// Mark a variant that carries a value with `(_)`. The payload's type is not
/// repeated here; it is already on the enum, and stating it twice would be a
/// second place to keep in step.
///
/// ```
/// #[derive(Default, PartialEq, Debug)]
/// struct Circle { radius: f64 }
/// structio::object!(Circle { radius });
///
/// #[derive(Default, PartialEq, Debug)]
/// enum Shape {
///     #[default]
///     Empty,
///     Circle(Circle),
///     Sides(u32),
/// }
///
/// structio::tagged_enum!(Shape {
///     Empty,
///     Circle(_),
///     Sides(_),
/// });
///
/// assert_eq!(structio::to_string(&Shape::Empty), "\"Empty\"");
/// assert_eq!(
///     structio::to_string(&Shape::Circle(Circle { radius: 2.0 })),
///     r#"{"Circle":{"radius":2}}"#
/// );
/// assert_eq!(
///     structio::from_str::<Shape>(r#"{"Sides":3}"#).unwrap(),
///     Shape::Sides(3)
/// );
/// ```
///
/// Names are renamed the same way a field is, they take a
/// [case rule](crate::case) the same way, and generics go in brackets before
/// the type, exactly as for [`object!`]:
///
/// ```
/// # #[derive(Default, PartialEq, Debug)]
/// # enum Message<T> { #[default] Ping, Data(T) }
/// structio::tagged_enum!([T: structio::ReadWrite + Default] Message<T> {
///     "ping" => Ping,
///     "data" => Data(_),
/// });
/// # assert_eq!(structio::to_string(&Message::Data(vec![1u8, 2])), r#"{"data":[1,2]}"#);
/// ```
///
/// # One payload, of a type you already declared
///
/// A variant carries at most one value, which is the shape a
/// `std::variant<A, B, C>` has and the one that composes: the payload is an
/// ordinary type, declared with [`object!`] or [`array!`] or built in, and the
/// enum adds only the tag. A Rust variant with several fields, or with named
/// fields, is not accepted; give it a struct or a tuple instead. Neither is
/// [`object!`]'s `#[required]` marker, which has nothing to say here: a variant
/// declared as carrying a value can be read only from the object form that
/// holds one, so its payload is required by the declaration itself.
///
/// A payload type needs [`Default`], for the reason an `Option`'s payload
/// does: reading a variant the destination is not already holding has to build
/// one before it can read into it.
///
/// ```
/// # #[derive(Default, PartialEq, Debug)]
/// # enum Span { #[default] None, Range((u32, u32)) }
/// structio::tagged_enum!(Span { None, Range(_) });
/// assert_eq!(structio::to_string(&Span::Range((1, 5))), r#"{"Range":[1,5]}"#);
/// ```
///
/// # What is read back
///
/// The two forms are not interchangeable, and the asymmetry runs one way. A
/// variant carrying nothing reads from either, so a producer that always
/// writes the object form still round-trips. A variant carrying a value has
/// only the object form: the name on its own leaves the value missing, which
/// is [`ExpectedBrace`](crate::ErrorCode::ExpectedBrace) in JSON and
/// [`ExpectedObject`](crate::ErrorCode::ExpectedObject) in BEVE rather than an
/// unknown variant, the name having been recognized and the value under it
/// not being there.
///
/// ```
/// # #[derive(Default, PartialEq, Debug)]
/// # enum Shape { #[default] Empty, Sides(u32) }
/// # structio::tagged_enum!(Shape { Empty, Sides(_) });
/// use structio::{ErrorCode, from_str};
///
/// // A variant carrying nothing takes either form.
/// assert_eq!(from_str::<Shape>("\"Empty\"").unwrap(), Shape::Empty);
/// assert_eq!(from_str::<Shape>(r#"{"Empty":null}"#).unwrap(), Shape::Empty);
///
/// // A variant carrying a value takes the object form and only that.
/// assert_eq!(from_str::<Shape>(r#"{"Sides":6}"#).unwrap(), Shape::Sides(6));
/// assert_eq!(
///     from_str::<Shape>("\"Sides\"").unwrap_err().code,
///     ErrorCode::ExpectedBrace,
/// );
/// ```
///
/// What is refused is a name no variant claims, and that is refused under
/// every policy, including [`SkipUnknown`](crate::SkipUnknown). Stepping over
/// an unknown object key still leaves the object readable; stepping over an
/// unknown variant would leave the value itself undecided.
///
/// An object that is not exactly one member, `{}` or two tags at once, and a
/// value that is neither an object nor a string, are
/// [`ExpectedVariant`](crate::ErrorCode::ExpectedVariant). Under the object
/// form a variant carrying nothing wants `null` specifically, so `{"Empty":0}`
/// is [`ExpectedNull`](crate::ErrorCode::ExpectedNull).
///
/// Reading reuses what the destination already holds when it is already the
/// variant being read, so a loop that reads the same variant repeatedly keeps
/// its payload's buffers. Reading a *different* variant replaces the value,
/// which is what changing variants means.
///
/// # What it expands to
///
/// One [`Variants`](crate::Variants) impl carrying the name list and its
/// compile-time perfect hash, and then, for each format, `ReadEnum` and
/// `Read`/`Write`. Each `write` ends with a `match` over every declared
/// variant whose arms are empty, dead code that asks the compiler to confirm
/// the declaration names every variant the enum has, so extending the enum
/// later and forgetting to say so here is a compile error rather than a value
/// that silently writes nothing.
///
/// When a payload's type cannot support both formats, declare the enum with
/// [`json_tagged_enum!`](crate::json_tagged_enum) or [`beve_tagged_enum!`](crate::beve_tagged_enum) instead.
///
/// # Further reading
///
/// [docs/enums.md](https://github.com/stephenberry/structio/blob/main/docs/enums.md) is the long form: every error an enum can
/// produce and what distinguishes it from the others, how the policies meet a
/// tag, generics and borrowed payloads, the string array a run of unit
/// variants becomes in BEVE, and how validation, pointers and transcoding walk
/// through one.
#[macro_export]
macro_rules! tagged_enum {
    ($($t:tt)*) => { $crate::__declare_enum!(__both_enum_impls $($t)*); };
}

/// Declare an enum for JSON alone.
///
/// The same syntax as [`tagged_enum!`](crate::tagged_enum), generating only the JSON impls. The
/// reasons to want it are [`json_object!`]'s.
#[macro_export]
macro_rules! json_tagged_enum {
    ($($t:tt)*) => { $crate::__declare_enum!(__json_enum_impls $($t)*); };
}

/// Declare an enum for BEVE alone.
///
/// The counterpart of [`json_tagged_enum!`](crate::json_tagged_enum), and what an enum with a borrowed
/// `&[u8]` payload needs.
#[macro_export]
macro_rules! beve_tagged_enum {
    ($($t:tt)*) => { $crate::__declare_enum!(__beve_enum_impls $($t)*); };
}

/// [`__declare!`](crate::__declare) for the enum forms.
///
/// The same normalization of `'de`, and the same two lists handed on. What it
/// shares across formats is [`Variants`](crate::Variants), the enum's
/// counterpart of [`Keys`](crate::Keys).
#[doc(hidden)]
#[macro_export]
macro_rules! __declare_enum {
    ($m:ident [ 'de $($gen:tt)* ] $ty:ty as $case:tt { $($body:tt)* }) => {
        $crate::__declared_enum!($m ['de $($gen)*] ['de $($gen)*] [$case] $ty { $($body)* });
    };
    ($m:ident [ 'de $($gen:tt)* ] $ty:ty { $($body:tt)* }) => {
        $crate::__declared_enum!($m ['de $($gen)*] ['de $($gen)*] [_] $ty { $($body)* });
    };
    ($m:ident [ $($gen:tt)* ] $ty:ty as $case:tt { $($body:tt)* }) => {
        $crate::__declared_enum!($m ['de, $($gen)*] [$($gen)*] [$case] $ty { $($body)* });
    };
    ($m:ident [ $($gen:tt)* ] $ty:ty { $($body:tt)* }) => {
        $crate::__declared_enum!($m ['de, $($gen)*] [$($gen)*] [_] $ty { $($body)* });
    };
    ($m:ident $ty:ty as $case:tt { $($body:tt)* }) => {
        $crate::__declared_enum!($m ['de] [] [$case] $ty { $($body)* });
    };
    ($m:ident $ty:ty { $($body:tt)* }) => {
        $crate::__declared_enum!($m ['de] [] [_] $ty { $($body)* });
    };
}

/// [`__declared!`](crate::__declared) for the enum forms.
#[doc(hidden)]
#[macro_export]
macro_rules! __declared_enum {
    ($m:ident [$($rgen:tt)*] [$($wgen:tt)*] [$case:tt] $ty:ty { $($body:tt)* }) => {
        $crate::__case_check!($case);
        $crate::__variants_impl!([$($wgen)*] [$case] $ty { $($body)* });
        $crate::$m!([$($rgen)*] [$($wgen)*] [$case] $ty { $($body)* });
    };
}

/// Both formats, for [`unit_enum!`](crate::unit_enum). The JSON half is the
/// tagged one unchanged; only BEVE has anything to add.
#[doc(hidden)]
#[macro_export]
macro_rules! __both_unit_enum_impls {
    ([$($rgen:tt)*] [$($wgen:tt)*] [$case:tt] $ty:ty { $($body:tt)* }) => {
        $crate::__json_enum_impls!([$($rgen)*] [$($wgen)*] [$case] $ty { $($body)* });
        $crate::__beve_unit_enum_impls!([$($rgen)*] [$($wgen)*] [$case] $ty { $($body)* });
    };
}

/// Both formats, for [`tagged_enum!`](crate::tagged_enum).
#[doc(hidden)]
#[macro_export]
macro_rules! __both_enum_impls {
    ([$($rgen:tt)*] [$($wgen:tt)*] [$case:tt] $ty:ty { $($body:tt)* }) => {
        $crate::__json_enum_impls!([$($rgen)*] [$($wgen)*] [$case] $ty { $($body)* });
        $crate::__beve_enum_impls!([$($rgen)*] [$($wgen)*] [$case] $ty { $($body)* });
    };
}

/// Refuse a variant payload spelled as anything but `(_)`.
///
/// The payload slot is a `tt` run, so without this it swallows whatever is put
/// between the parentheses and generates code that ignores it. The case that
/// matters is `Variant(_ as Adapter)`: adapters are a field-level feature that
/// [`tagged_enum!`](crate::tagged_enum) deliberately does not have yet, and a
/// user who has just met `field as Adapter` on [`object!`](crate::object) will
/// reach for it here. Silently writing the payload through its own `Write`
/// would be a document that is valid, wrong, and unremarked.
///
/// A `compile_error!` rather than a matcher that simply fails to match, for
/// the reason `unit_enum!`'s payload rejection uses one: a matcher error is
/// pointed into this crate rather than at the declaration that caused it.
#[doc(hidden)]
#[macro_export]
macro_rules! __payload_is_wildcard {
    () => {};
    (_) => {};
    ($($other:tt)*) => {
        ::core::compile_error!(
            "structio: a variant payload is written `(_)`, and carries no options of its own. \
             Field adapters (`as ...`) are supported on `object!` fields, not on enum payloads."
        );
    };
}

/// The format-independent half: the variant names and their compile-time hash.
#[doc(hidden)]
#[macro_export]
macro_rules! __variants_impl {
    (
        [$($wgen:tt)*] [$case:tt] $ty:ty {
            $($($name:literal =>)? $variant:ident $(($($payload:tt)*))?),* $(,)?
        }
    ) => {
        $crate::__each_name_once!($($variant),*);
        $( $crate::__payload_is_wildcard!($($($payload)*)?); )*

        impl<$($wgen)*> $crate::Variants for $ty {
            const VARIANTS: &'static [&'static str] = &[
                $( $crate::__json_key!([$case] $($name)? [$variant]) ),*
            ];
            // Built from `Self::VARIANTS`, and promoted to an anonymous static,
            // for the reasons `Keys::MAP` is.
            const MAP: &'static $crate::KeyMap = &$crate::KeyMap::build(Self::VARIANTS);
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __json_enum_impls {
    (
        [$($rgen:tt)*] [$($wgen:tt)*] [$case:tt] $ty:ty {
            $($($name:literal =>)? $variant:ident $(($($payload:tt)*))?),* $(,)?
        }
    ) => {
        impl<$($rgen)*> $crate::json::ReadEnum<'de> for $ty {
            // Deliberately not `inline(always)`, for `read_field`'s reason:
            // this body holds an arm for every variant.
            #[inline]
            #[allow(unused_assignments, unused_variables, unused_mut)]
            fn read_name<O: $crate::Options>(
                &mut self,
                index: usize,
                p: &mut $crate::json::Parser<'de, O>,
            ) -> ::core::result::Result<bool, $crate::ErrorCode> {
                // The same const-folding counter `read_field` uses, and for the
                // same reason: expansion stays linear in the variant count.
                let mut i = 0usize;
                $(
                    if index == i {
                        return $crate::__json_read_name!(
                            self, p,
                            $crate::__json_key!([$case] $($name)? [$variant]),
                            $variant $(($($payload)*))?
                        );
                    }
                    i += 1;
                )*
                ::core::result::Result::Ok(false)
            }

            #[inline]
            #[allow(unused_assignments, unused_variables, unused_mut, unreachable_patterns)]
            fn read_payload<O: $crate::Options>(
                &mut self,
                index: usize,
                p: &mut $crate::json::Parser<'de, O>,
            ) -> ::core::result::Result<bool, $crate::ErrorCode> {
                let mut i = 0usize;
                $(
                    if index == i {
                        return $crate::__json_read_payload!(
                            self, p,
                            $crate::__json_key!([$case] $($name)? [$variant]),
                            $variant $(($($payload)*))?
                        );
                    }
                    i += 1;
                )*
                ::core::result::Result::Ok(false)
            }
        }

        impl<$($rgen)*> $crate::json::Read<'de> for $ty {
            #[inline]
            fn read<O: $crate::Options>(
                &mut self,
                p: &mut $crate::json::Parser<'de, O>,
            ) -> ::core::result::Result<(), $crate::ErrorCode> {
                p.read_enum(self)
            }
        }

        impl<$($wgen)*> $crate::json::Write for $ty {
            #[inline]
            #[allow(irrefutable_let_patterns)]
            fn write<O: $crate::Options>(&self, w: &mut $crate::json::Writer<'_, O>) {
                // The duplicate-name check is in `KeyMap::build`, which nothing
                // reaches but `Variants::MAP`. Reading looks a name up and so
                // evaluates it; writing has no use for it, and a generic
                // type's associated const is evaluated only when something
                // names it, so a generic declaration that is never read would
                // otherwise write two variants under one name. Naming it here
                // costs nothing and closes that.
                const {
                    let _ = <Self as $crate::Variants>::MAP;
                };
                $(
                    $crate::__write_variant!(
                        self, w,
                        $crate::__json_key!([$case] $($name)? [$variant]),
                        $crate::__json_member!([$case] $($name)? [$variant]),
                        $variant $(($($payload)*))?
                    );
                )*
                // Every variant returned above. This says so to the compiler,
                // which is what makes extending the enum without extending the
                // declaration a build error instead of a value that writes
                // nothing at all.
                match self { $( Self::$variant { .. } => {} ),* }
            }
        }
    };
}

/// One arm of the JSON `read_name`: the bare-name form.
///
/// `macro_rules!` cannot branch inside a repetition, so everywhere the two
/// kinds of variant differ goes through a helper with one rule for each. This
/// is the first of five, and the rest follow its shape.
#[doc(hidden)]
#[macro_export]
macro_rules! __json_read_name {
    // Carries nothing, so the name is the whole value.
    ($self:ident, $p:ident, $name:expr, $variant:ident) => {
        if $p.match_key($name) {
            *$self = Self::$variant;
            ::core::result::Result::Ok(true)
        } else {
            ::core::result::Result::Ok(false)
        }
    };
    // Carries a value, which the bare form has nowhere to put. The name was
    // recognized, so this is not an unknown variant: what is missing is the
    // object that would have held the value.
    ($self:ident, $p:ident, $name:expr, $variant:ident ($($payload:tt)*)) => {
        if $p.match_key($name) {
            ::core::result::Result::Err($crate::ErrorCode::ExpectedBrace)
        } else {
            ::core::result::Result::Ok(false)
        }
    };
}

/// One arm of the JSON `read_payload`: the single member of an object.
#[doc(hidden)]
#[macro_export]
macro_rules! __json_read_payload {
    // Carries nothing. Written as a bare name, but accepted here as well, so a
    // producer that always writes the object form round-trips.
    ($self:ident, $p:ident, $name:expr, $variant:ident) => {
        if $p.match_key($name) {
            $p.colon()?;
            if $p.try_null()? {
                *$self = Self::$variant;
                ::core::result::Result::Ok(true)
            } else {
                // The name is this variant's, so the value is not a tag that
                // went unrecognized: it is one that named a variant carrying
                // nothing and then put something under it.
                ::core::result::Result::Err($crate::ErrorCode::ExpectedNull)
            }
        } else {
            ::core::result::Result::Ok(false)
        }
    };
    ($self:ident, $p:ident, $name:expr, $variant:ident ($($payload:tt)*)) => {
        if $p.match_key($name) {
            $p.colon()?;
            // Read into what is already there when it is already this variant,
            // so a payload's buffers survive the read the way a struct field's
            // do. Anything else is replaced, which is what changing variants
            // means.
            match $self {
                Self::$variant(v) => {
                    $crate::json::Read::read(v, $p)?;
                }
                _ => {
                    let mut value = ::core::default::Default::default();
                    $crate::json::Read::read(&mut value, $p)?;
                    *$self = Self::$variant(value);
                }
            }
            ::core::result::Result::Ok(true)
        } else {
            ::core::result::Result::Ok(false)
        }
    };
}

/// One arm of `write`, in either format.
///
/// The one helper the two formats share, because the two calls are spelled the
/// same in both: a variant carrying nothing is its name, and one carrying a
/// value is a tag. Only the pre-encoded key differs, so it is passed in rather
/// than built here.
#[doc(hidden)]
#[macro_export]
macro_rules! __write_variant {
    ($self:ident, $w:ident, $name:expr, $key:expr, $variant:ident) => {
        if let Self::$variant = $self {
            $w.write_str($name);
            return;
        }
    };
    ($self:ident, $w:ident, $name:expr, $key:expr, $variant:ident ($($payload:tt)*)) => {
        if let Self::$variant(v) = $self {
            $w.write_tagged($key, v);
            return;
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __beve_enum_impls {
    (
        [$($rgen:tt)*] [$($wgen:tt)*] [$case:tt] $ty:ty {
            $($($name:literal =>)? $variant:ident $(($($payload:tt)*))?),* $(,)?
        }
    ) => {
        $crate::__beve_enum_body!([$($rgen)*] [$($wgen)*] [$case] $ty {
            $($($name =>)? $variant $(($($payload)*))?),*
        } {});
    };
}

/// The BEVE impls for [`unit_enum!`](crate::unit_enum), which are the tagged
/// ones plus a typed array.
///
/// A unit enum's value is a string and can be nothing else, so a run of them is
/// a **string array**: one header for the lot and a length-prefixed name per
/// element, exactly as a `Vec<String>` is stored, rather than a generic array
/// carrying a string header per element. Nothing on the reading side changes,
/// since the sequence driver installs a typed array's element header and hands
/// out one value either way, so the two forms stay interchangeable.
///
/// [`tagged_enum!`](crate::tagged_enum) cannot do this even for a declaration
/// that happens to be all unit variants: `ARRAY` is one constant for the type,
/// and a variant carrying a value writes an object.
#[doc(hidden)]
#[macro_export]
macro_rules! __beve_unit_enum_impls {
    (
        [$($rgen:tt)*] [$($wgen:tt)*] [$case:tt] $ty:ty {
            $($($name:literal =>)? $variant:ident),* $(,)?
        }
    ) => {
        $crate::__beve_enum_body!([$($rgen)*] [$($wgen)*] [$case] $ty {
            $($($name =>)? $variant),*
        } {
            const ARRAY: ::core::option::Option<&'static [u8]> =
                ::core::option::Option::Some(&[$crate::beve::header::STRING_ARRAY]);

            #[inline]
            fn write_payload<O: $crate::Options>(
                items: &[Self],
                w: &mut $crate::beve::Writer<'_, O>,
            ) where
                Self: ::core::marker::Sized,
            {
                // The header and count are already out, so an element is its
                // name with a length in front and nothing else. A real `match`
                // here, where `write` needs a chain, because this macro's own
                // matcher has already refused a variant that carries a value:
                // every arm has the same shape, so nothing has to branch and
                // the compiler checks the arms cover the enum.
                for item in items {
                    w.write_str_body(match item {
                        $( Self::$variant => $crate::__json_key!([$case] $($name)? [$variant]) ),*
                    });
                }
            }
        });
    };
}

/// The BEVE impls themselves, with `$typed` holding the two items that make a
/// run of this enum a string array, or nothing.
#[doc(hidden)]
#[macro_export]
macro_rules! __beve_enum_body {
    (
        [$($rgen:tt)*] [$($wgen:tt)*] [$case:tt] $ty:ty {
            $($($name:literal =>)? $variant:ident $(($($payload:tt)*))?),* $(,)?
        } { $($typed:tt)* }
    ) => {
        impl<$($rgen)*> $crate::beve::ReadEnum<'de> for $ty {
            #[inline]
            #[allow(unused_assignments, unused_variables, unused_mut)]
            fn read_name(
                &mut self,
                index: usize,
                name: &[u8],
            ) -> ::core::result::Result<bool, $crate::ErrorCode> {
                let mut i = 0usize;
                $(
                    if index == i {
                        return $crate::__beve_read_name!(
                            self, name,
                            $crate::__json_key!([$case] $($name)? [$variant]),
                            $variant $(($($payload)*))?
                        );
                    }
                    i += 1;
                )*
                ::core::result::Result::Ok(false)
            }

            #[inline]
            #[allow(unused_assignments, unused_variables, unused_mut, unreachable_patterns)]
            fn read_payload<O: $crate::Options>(
                &mut self,
                index: usize,
                name: &[u8],
                r: &mut $crate::beve::Reader<'de, O>,
            ) -> ::core::result::Result<bool, $crate::ErrorCode> {
                let mut i = 0usize;
                $(
                    if index == i {
                        return $crate::__beve_read_payload!(
                            self, name, r,
                            $crate::__json_key!([$case] $($name)? [$variant]),
                            $variant $(($($payload)*))?
                        );
                    }
                    i += 1;
                )*
                ::core::result::Result::Ok(false)
            }
        }

        impl<$($rgen)*> $crate::beve::Read<'de> for $ty {
            #[inline]
            fn read<O: $crate::Options>(
                &mut self,
                r: &mut $crate::beve::Reader<'de, O>,
            ) -> ::core::result::Result<(), $crate::ErrorCode> {
                r.read_enum(self)
            }
        }

        impl<$($wgen)*> $crate::beve::Write for $ty {
            #[inline]
            #[allow(irrefutable_let_patterns)]
            fn write<O: $crate::Options>(&self, w: &mut $crate::beve::Writer<'_, O>) {
                // The duplicate-name check is in `KeyMap::build`, which nothing
                // reaches but `Variants::MAP`. Reading looks a name up and so
                // evaluates it; writing has no use for it, and a generic
                // type's associated const is evaluated only when something
                // names it, so a generic declaration that is never read would
                // otherwise write two variants under one name. Naming it here
                // costs nothing and closes that.
                const {
                    let _ = <Self as $crate::Variants>::MAP;
                };
                $(
                    $crate::__write_variant!(
                        self, w,
                        $crate::__json_key!([$case] $($name)? [$variant]),
                        $crate::__beve_key_bytes!($crate::__json_key!([$case] $($name)? [$variant])),
                        $variant $(($($payload)*))?
                    );
                )*
                // As on the JSON side: the compiler's word that the declaration
                // names every variant.
                match self { $( Self::$variant { .. } => {} ),* }
            }

            $($typed)*
        }
    };
}

/// One arm of the BEVE `read_name`.
#[doc(hidden)]
#[macro_export]
macro_rules! __beve_read_name {
    ($self:ident, $key:ident, $name:expr, $variant:ident) => {
        if $key == $name.as_bytes() {
            *$self = Self::$variant;
            ::core::result::Result::Ok(true)
        } else {
            ::core::result::Result::Ok(false)
        }
    };
    ($self:ident, $key:ident, $name:expr, $variant:ident ($($payload:tt)*)) => {
        if $key == $name.as_bytes() {
            ::core::result::Result::Err($crate::ErrorCode::ExpectedObject)
        } else {
            ::core::result::Result::Ok(false)
        }
    };
}

/// One arm of the BEVE `read_payload`.
#[doc(hidden)]
#[macro_export]
macro_rules! __beve_read_payload {
    ($self:ident, $key:ident, $r:ident, $name:expr, $variant:ident) => {
        if $key == $name.as_bytes() {
            if $r.try_null()? {
                *$self = Self::$variant;
                ::core::result::Result::Ok(true)
            } else {
                ::core::result::Result::Err($crate::ErrorCode::ExpectedNull)
            }
        } else {
            ::core::result::Result::Ok(false)
        }
    };
    ($self:ident, $key:ident, $r:ident, $name:expr, $variant:ident ($($payload:tt)*)) => {
        if $key == $name.as_bytes() {
            match $self {
                Self::$variant(v) => {
                    $crate::beve::Read::read(v, $r)?;
                }
                _ => {
                    let mut value = ::core::default::Default::default();
                    $crate::beve::Read::read(&mut value, $r)?;
                    *$self = Self::$variant(value);
                }
            }
            ::core::result::Result::Ok(true)
        } else {
            ::core::result::Result::Ok(false)
        }
    };
}
