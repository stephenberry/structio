//! The JSON half of the trait set.
//!
//! [`Read`] and [`Write`] are implemented for every supported type.
//! [`ReadObject`] and [`WriteObject`] describe a struct's fields against the
//! shared [`Keys`] schema, and are what the [`object!`](crate::object) macro
//! generates. [`ReadArray`] and [`WriteArray`] are their positional
//! counterparts, from [`array!`](crate::array). [`ReadEnum`] is the same idea
//! against the [`Variants`] schema, from
//! [`tagged_enum!`](crate::tagged_enum); it has no writing half, because a
//! variant is written by one call rather than by a callback over its parts.
//!
//! [`ReadAs`] and [`WriteAs`] are the same pair as [`Read`] and [`Write`],
//! moved off the type and onto an *adapter*, which is what lets a field keep a
//! type from a crate you do not own. [`ReadKeyAs`] and [`WriteKeyAs`] are the
//! same idea for a map's keys, which go through
//! [`FromJsonKey`](crate::json::FromJsonKey) rather than through [`Read`].
//!
//! Implementing these by hand is fully supported and is the escape hatch for
//! anything the macro cannot express. The macro exists only to remove the
//! boilerplate.

use crate::error::PResult;
use crate::json::parser::Parser;
use crate::json::writer::Writer;
use crate::options::Options;
use crate::traits::{Elements, Keys, Variants};

/// A type that can be parsed from JSON.
///
/// Reading is into an existing value rather than returning a new one, so
/// buffers and allocations already held by the destination get reused. This is
/// the same reason Glaze reads into a reference.
///
/// The `'de` lifetime is the input document's. A type that borrows from the
/// input, such as `&'de str`, ties itself to it; an owning type ignores it.
pub trait Read<'de>: Sized {
    /// Parse into `self`, from the cursor's current position.
    ///
    /// Generic over the [read policy](crate::Options) for the same reason
    /// [`Write::write`] is: it keeps a bound on a container element spelled
    /// `T: Read<'de>` rather than `T: Read<'de, O>`. `O` is inferred from the
    /// parser, so an implementation forwards `p` on and never names it unless
    /// it wants to read a setting.
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()>;
}

/// A type that can be serialized to JSON.
///
/// The method is generic over the [write policy](crate::Options) rather than
/// the trait being generic over it, which is what keeps a bound on a container
/// element spelled `T: Write` instead of `T: Write<O>`. An implementation
/// forwards `w` on and never names `O` unless it wants to read a setting.
pub trait Write {
    fn write<O: Options>(&self, w: &mut Writer<'_, O>);

    /// Whether this value is absent, and so is left out of an object under
    /// [`Options::SKIP_NULL`].
    ///
    /// Absence, not the bytes: a NaN `f64` writes as `null` because JSON has
    /// no other form for it, and is still a number that is present. The
    /// default is `false`, which is right for everything that holds a value. `Option` overrides it, `()` overrides it, and the
    /// wrappers forward it, so a `Box<Option<T>>` holding `None` is absent for
    /// the same reason a bare `None` is.
    ///
    /// Only [`Writer::member`] consults this; a member written through an
    /// adapter asks [`WriteAs::is_null`] instead, so the adapter has the
    /// answer for the type it describes. A null inside a sequence or a map is
    /// written out either way, since dropping it would change the data rather
    /// than its presentation.
    #[inline]
    fn is_null(&self) -> bool {
        false
    }
}

/// How a field of type `T` is read when its declaration names this adapter.
///
/// Implemented by the adapter rather than by `T`, which is what lets it
/// describe a type from another crate: the adapter is local to whoever writes
/// the impl, so the orphan rule is satisfied wherever it lives. The field keeps
/// its own type; only the reading of it moves.
///
/// ```
/// # use std::time::Duration;
/// # use structio::{ErrorCode, Options, json};
/// struct Millis;
///
/// impl<'de> json::ReadAs<'de, Duration> for Millis {
///     fn read<O: Options>(
///         value: &mut Duration,
///         p: &mut json::Parser<'de, O>,
///     ) -> Result<(), ErrorCode> {
///         let mut ms = 0u64;
///         json::Read::read(&mut ms, p)?;
///         *value = Duration::from_millis(ms);
///         Ok(())
///     }
/// }
/// ```
///
/// Adapters compose, because an adapter is a type: `Option<Millis>` reads an
/// `Option<Duration>` and `Vec<Millis>` reads a `Vec<Duration>`, each mirroring
/// the container's own [`Read`] impl. [`Same`](crate::Same) is the identity,
/// for a position that wants the type's own impl inside one that does not.
pub trait ReadAs<'de, T> {
    /// Read into `value`, from the cursor's current position.
    ///
    /// The same contract as [`Read::read`], including that the destination is
    /// reused rather than replaced: an adapter over a `String`-shaped type
    /// should refill it.
    ///
    /// Unlike [`ReadObject::read_field`], the neighbour an adapter author is
    /// most likely to copy, this has no way to decline. It is reached only
    /// after the key and its colon have both been consumed, so the member is
    /// known to be this one and a failure here is terminal. The error is
    /// reported against the object rather than wherever it was noticed,
    /// exactly as it is for a hand-written impl.
    fn read<O: Options>(value: &mut T, p: &mut Parser<'de, O>) -> PResult<()>;
}

/// How a field of type `T` is written when its declaration names this adapter.
///
/// The writing half of [`ReadAs`], split from it for the reason [`Read`] and
/// [`Write`] are split: `'de` belongs to the read half alone.
pub trait WriteAs<T: ?Sized> {
    /// Write `value`.
    ///
    /// Like [`Write::write`] this cannot fail, which for a native type is
    /// simply true and for a foreign one is a constraint: an adapter whose
    /// target has values it cannot encode must either write a documented
    /// substitute or panic, and must say in its own documentation which.
    ///
    /// A composite value is assembled through [`Writer::write_object`],
    /// [`Writer::write_seq`], [`Writer::write_seq_with`] or
    /// [`Writer::write_keyed_with`] rather than by writing brackets and keys
    /// directly, which the writer does not expose.
    fn write<O: Options>(value: &T, w: &mut Writer<'_, O>);

    /// Whether the field is absent, and so is left out of an object under
    /// [`Options::SKIP_NULL`].
    ///
    /// The adapter's answer rather than the value's, since the adapter is what
    /// decides what the value means on the wire. The default is `false`, for
    /// the same reason [`Write::is_null`]'s is.
    ///
    /// [`Writer::member_with`] is the only thing that acts on it, and the
    /// composed adapters forward it the way the wrappers forward
    /// [`Write::is_null`]: `Option<A>` is absent when it is `None` and defers
    /// to `A` otherwise, and `Box<A>`, `Rc<A>` and `Arc<A>` pass it through.
    #[inline]
    fn is_null(value: &T) -> bool {
        let _ = value;
        false
    }
}

/// How a map key of type `T` is read when its declaration names this adapter.
///
/// A key is not a value: it never passes through [`Read`] at all, because a
/// JSON key is always a string and a numeric key is parsed out of the quoted
/// text. So a key position takes its own adapter trait, and
/// `HashMap<KA, VA>` adapts a `HashMap<K, V>` by naming one for each half.
pub trait ReadKeyAs<T> {
    /// Convert an unescaped key to the key type, the counterpart of
    /// [`FromJsonKey::from_key`](crate::json::FromJsonKey::from_key).
    fn from_key(key: &str) -> PResult<T>;
}

/// How a map key of type `T` is written when its declaration names this
/// adapter.
pub trait WriteKeyAs<T: ?Sized> {
    /// Write the key, quotes included, and nothing after it.
    ///
    /// The counterpart of
    /// [`ToJsonKey::write_key`](crate::json::ToJsonKey::write_key): the colon
    /// is the caller's.
    fn write_key<O: Options>(value: &T, w: &mut Writer<'_, O>);
}

/// Field-by-field reading for a struct.
pub trait ReadObject<'de>: Keys + Sized {
    /// Parse the value for field `index`.
    ///
    /// The cursor sits on the first byte of the key. The implementation must
    /// confirm the key with [`Parser::match_key`] before parsing anything,
    /// because `index` comes from a hash and is only a candidate.
    ///
    /// Returns `false` if the key did not match, leaving the cursor untouched
    /// so the caller can treat the member as unknown: under
    /// [`Options::ERROR_ON_UNKNOWN_KEYS`] that is an
    /// [`ErrorCode::UnknownKey`](crate::ErrorCode::UnknownKey), and otherwise
    /// the member is stepped over.
    ///
    /// Returning `true` is also what records the field as filled, which is
    /// what [`Options::ERROR_ON_MISSING_KEYS`] checks the object against.
    fn read_field<O: Options>(&mut self, index: usize, p: &mut Parser<'de, O>) -> PResult<bool>;
}

/// Field-by-field writing for a struct.
pub trait WriteObject: Keys {
    /// Write every member as `"key":value,` including the trailing comma.
    ///
    /// The caller overwrites the final comma with the closing brace, which is
    /// why no member has to test whether it is first.
    ///
    /// Members go through [`Writer::member`], which is what applies
    /// [`Options::SKIP_NULL`]. An implementation that writes a member some
    /// other way opts out of that, which is allowed and occasionally wanted.
    fn write_fields<O: Options>(&self, w: &mut Writer<'_, O>);
}

/// Element-by-element reading for a struct written as a JSON array.
///
/// The positional counterpart of [`ReadObject`]. There is no key to confirm,
/// because position *is* the key: element `i` is field `i`, and a document
/// holding some other number of them is an error rather than a struct with
/// defaults in the gaps.
pub trait ReadArray<'de>: Elements + Sized {
    /// Parse the value at position `index`.
    ///
    /// The cursor sits on the first byte of the element. `index` counts from
    /// zero and is not bounded by [`Elements::LEN`]: an array longer than the
    /// struct reaches here with an index past the last field, which is an
    /// [`ErrorCode::ArrayLengthMismatch`](crate::ErrorCode::ArrayLengthMismatch).
    fn read_element<O: Options>(&mut self, index: usize, p: &mut Parser<'de, O>) -> PResult<()>;
}

/// Element-by-element writing for a struct written as a JSON array.
pub trait WriteArray: Elements {
    /// Write every element as `value,` including the trailing comma.
    ///
    /// The caller overwrites the final comma with the closing bracket, the
    /// same trick [`WriteObject::write_fields`] plays with the brace.
    fn write_elements<O: Options>(&self, w: &mut Writer<'_, O>);
}

/// Variant-by-variant reading for an enum.
///
/// The counterpart of [`ReadObject`] for a type declared with
/// [`unit_enum!`](crate::unit_enum) or [`tagged_enum!`](crate::tagged_enum).
/// There are two methods because there are two forms on the wire, and
/// [`Parser::read_enum`] has already decided which one it is looking at: a
/// bare name reaches [`read_name`](Self::read_name), and the single key of an
/// object reaches [`read_payload`](Self::read_payload).
///
/// `index` is only the candidate the hash proposed, exactly as in
/// [`ReadObject::read_field`], so both must confirm the name with
/// [`Parser::match_key`] before doing anything else.
pub trait ReadEnum<'de>: Variants + Sized {
    /// Take variant `index` written as a bare name, the cursor sitting on the
    /// first byte of it.
    ///
    /// Returns `false` if the name did not match, leaving the cursor
    /// untouched, which the caller reports as
    /// [`ErrorCode::UnknownVariant`](crate::ErrorCode::UnknownVariant). A
    /// variant that carries a value has no bare form and answers with
    /// [`ErrorCode::ExpectedBrace`](crate::ErrorCode::ExpectedBrace) instead:
    /// its name was recognized, and what is missing is the value under it.
    fn read_name<O: Options>(&mut self, index: usize, p: &mut Parser<'de, O>) -> PResult<bool>;

    /// Take variant `index` written as the single key of an object, the cursor
    /// sitting on the first byte of that key.
    ///
    /// The implementation consumes the key, the colon, and the value. A
    /// variant that carries nothing accepts `null` here, so a producer that
    /// always writes the object form still round-trips.
    ///
    /// Returns `false` if the name did not match, with the same meaning it has
    /// for [`read_name`](Self::read_name).
    fn read_payload<O: Options>(&mut self, index: usize, p: &mut Parser<'de, O>) -> PResult<bool>;
}

/// Convenience bound for generic containers: readable from any JSON input, and
/// writable.
///
/// Types that borrow from the input do not satisfy this, exactly as they do
/// not satisfy an "owned" bound elsewhere in the ecosystem. Prefer
/// [`crate::ReadWrite`], which also covers BEVE, unless the type is deliberately
/// JSON only.
pub trait ReadWrite: for<'de> Read<'de> + Write {}
impl<T> ReadWrite for T where T: for<'de> Read<'de> + Write {}
