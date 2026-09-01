//! The BEVE half of the trait set.
//!
//! The shape mirrors [`json`](crate::json) exactly: [`Read`] and [`Write`] for
//! every supported type, [`ReadObject`] and [`WriteObject`] for a struct's
//! fields against the shared [`Keys`] schema, and [`ReadArray`] and
//! [`WriteArray`] for one written positionally instead. A type declared with
//! [`object!`](crate::object) or [`array!`](crate::array) gets both sets at
//! once. [`ReadEnum`] is the same idea against the [`Variants`] schema, from
//! [`tagged_enum!`](crate::tagged_enum).
//!
//! [`ReadAs`] and [`WriteAs`] are the same pair as [`Read`] and [`Write`],
//! moved off the type and onto an *adapter*, which is what lets a field keep a
//! type from a crate you do not own. [`ReadKeyAs`] and [`WriteKeyAs`] are the
//! same idea for a map's keys, which go through
//! [`FromBeveKey`](crate::beve::FromBeveKey) rather than through [`Read`].
//!
//! The one thing BEVE adds is [`Write::ARRAY`]. JSON has a single array syntax,
//! so a sequence never has to ask what its elements are. BEVE stores a run of
//! numbers as a contiguous block with one header for the lot, which is most of
//! why it is worth having, so a sequence does have to ask. A run of
//! [`Complex`](crate::Complex) answers the same way, through the complex
//! extension.

use crate::beve::reader::{Key, Reader};
use crate::beve::writer::Writer;
use crate::error::PResult;
use crate::options::Options;
use crate::traits::{Elements, Keys, Variants};

/// A type that can be parsed from BEVE.
///
/// Reading is into an existing value, so buffers already held by the
/// destination get reused, exactly as on the JSON side.
///
/// The `'de` lifetime is the input buffer's. BEVE strings carry no escapes, so
/// a `&'de str` field always borrows and never has to fall back to a copy.
pub trait Read<'de>: Sized {
    /// Read into `self`, from the reader's current position.
    ///
    /// Generic over the [read policy](crate::Options) for the same reason
    /// [`Write::write`] is: it keeps a bound on a container element spelled
    /// `T: Read<'de>` rather than `T: Read<'de, O>`. `O` is inferred from the
    /// reader, so an implementation forwards `r` on and never names it unless
    /// it wants to read a setting.
    fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()>;

    /// Fill `out` from the payload of a typed array of `n` elements whose
    /// element header is `elem`, or return `false` to decline.
    ///
    /// This is the bulk path: when the stored element type is exactly this
    /// one, and the host is little endian, the payload is already the
    /// in-memory representation of `[Self]` and the whole array is one copy.
    /// Declining is always safe; the caller falls back to reading element by
    /// element, which is also what handles a stored width that differs from
    /// this type's.
    ///
    /// Implementations must consume from `r` only when they return `true`.
    #[doc(hidden)]
    fn read_bulk<O: Options>(
        _out: &mut Vec<Self>,
        _n: usize,
        _elem: u8,
        _r: &mut Reader<'de, O>,
    ) -> PResult<bool> {
        Ok(false)
    }
}

/// A type that can be serialized to BEVE.
///
/// The method is generic over the [write policy](crate::Options) rather than
/// the trait being generic over it, so a bound on a container element stays
/// `T: Write` instead of `T: Write<O>`.
pub trait Write {
    fn write<O: Options>(&self, w: &mut Writer<'_, O>);

    /// Whether this value is absent, and so is left out of an object under
    /// [`Options::SKIP_NULL`].
    ///
    /// The BEVE counterpart of
    /// [`json::Write::is_null`](crate::json::Write::is_null), and true for the
    /// same values: `None`, `()`, and the wrappers around them. The two are
    /// separate traits, so a type could disagree between formats, but nothing
    /// in this crate does and a schema is easier to reason about when a field
    /// is either present in both encodings or absent from both. The same
    /// caution applies with more force to an adapter, where one name at the
    /// field site covers both [`WriteAs::is_null`] impls and a disagreement
    /// between them is invisible at the declaration.
    #[inline]
    fn is_null(&self) -> bool {
        false
    }

    /// The header bytes a contiguous run of this type is stored under, or
    /// `None` if it has no array form of its own and belongs in a generic one.
    ///
    /// `Some` means a `Vec` or slice is written as this prefix, one count, and
    /// one block of payload rather than as a value per element. Which array it
    /// is does not matter here: the bytes say it, and
    /// [`write_payload`](Write::write_payload) knows how to fill it.
    ///
    /// One byte for every array BEVE's core defines, and two for the complex
    /// extension, which needs its extension header and then the class header
    /// saying what a component is. Nothing longer means anything. Every call
    /// site has this as a constant, so a one-byte prefix still lowers to a
    /// single store.
    #[doc(hidden)]
    const ARRAY: Option<&'static [u8]> = None;

    /// Append `items` as the payload of a typed array whose header and count
    /// the caller has already written.
    ///
    /// Only reached when [`Write::ARRAY`] is `Some`, and it is a constant, so
    /// the call folds away entirely for the types that have no typed array.
    #[doc(hidden)]
    fn write_payload<O: Options>(items: &[Self], w: &mut Writer<'_, O>)
    where
        Self: Sized,
    {
        let _ = (items, w);
        unreachable!("structio: a typed array without a payload writer")
    }
}

/// How a field of type `T` is read when its declaration names this adapter.
///
/// The BEVE half of [`json::ReadAs`](crate::json::ReadAs), and the same idea:
/// the impl is on the adapter rather than on `T`, so a type from another crate
/// can be described without a newtype around it. A declaration names one
/// adapter for both formats, so an adapter used from [`object!`](crate::object)
/// needs this impl as well as the JSON one; one used from
/// [`beve_object!`](crate::beve_object) needs only this.
pub trait ReadAs<'de, T> {
    /// Read into `value`, from the reader's current position.
    ///
    /// The same contract as [`Read::read`], including that the destination is
    /// reused rather than replaced. Unlike [`ReadObject::read_field`] there is
    /// no way to decline: the member is already known to be this one.
    fn read<O: Options>(value: &mut T, r: &mut Reader<'de, O>) -> PResult<()>;
}

/// How a field of type `T` is written when its declaration names this adapter.
pub trait WriteAs<T: ?Sized> {
    /// Write `value`.
    ///
    /// Like [`Write::write`] this cannot fail. A composite value is assembled
    /// through the writer's own methods, [`Writer::write_slice_with`] and
    /// [`Writer::write_keyed_with`] among them.
    fn write<O: Options>(value: &T, w: &mut Writer<'_, O>);

    /// Whether the field is absent, and so is left out of an object under
    /// [`Options::SKIP_NULL`].
    ///
    /// The adapter's answer rather than the value's. On this side it is load
    /// bearing twice over: [`Writer::member_with`] drops the member, and the
    /// generated [`WriteObject::count_fields`] subtracts it from the count the
    /// object header already stated. Both ask this one function about the same
    /// value, so an implementation that answers the same way twice cannot make
    /// them disagree; one that does not is the corruption the debug assertion
    /// in [`Writer::write_object`] exists to catch.
    #[inline]
    fn is_null(value: &T) -> bool {
        let _ = value;
        false
    }

    /// The adapter's answer to [`Write::ARRAY`]: the header bytes a contiguous
    /// run of adapted values is stored under, or `None` for a generic array.
    ///
    /// Without this an adapted `Vec` would lose the typed array its elements
    /// belong in, which is not an error but is a bigger document. It is why
    /// [`Same`](crate::Same) forwards `<T as Write>::ARRAY` and a `Vec<Same>`
    /// is byte-identical to the field it wraps.
    #[doc(hidden)]
    const ARRAY: Option<&'static [u8]> = None;

    /// Append `items` as the payload of a typed array whose header and count
    /// the caller has already written.
    ///
    /// Only reached when [`WriteAs::ARRAY`] is `Some`, and it is a constant, so
    /// the call folds away entirely for an adapter that has no typed array.
    #[doc(hidden)]
    fn write_payload<O: Options>(items: &[T], w: &mut Writer<'_, O>)
    where
        T: Sized,
    {
        let _ = (items, w);
        unreachable!("structio: a typed array without a payload writer")
    }
}

/// How a map key of type `T` is read when its declaration names this adapter.
///
/// A key is not a value: it goes through
/// [`FromBeveKey`](crate::beve::FromBeveKey) rather than through [`Read`], so
/// a key position takes its own adapter trait.
pub trait ReadKeyAs<T> {
    /// Convert a key to the key type, the counterpart of
    /// [`FromBeveKey::from_key`](crate::beve::FromBeveKey::from_key).
    fn from_key(key: Key<'_>) -> PResult<T>;
}

/// How a map key of type `T` is written when its declaration names this
/// adapter.
pub trait WriteKeyAs<T: ?Sized> {
    /// The object header these keys are stored under, the same constant
    /// [`ToBeveKey::OBJECT`](crate::beve::ToBeveKey::OBJECT) carries.
    ///
    /// BEVE distinguishes a string-keyed object from an integer-keyed one in
    /// the header byte, before any key is written, so an adapter that changes
    /// what a key is has to say which kind of object it is making.
    const OBJECT: u8;

    /// Write the key, its length prefix or width included, and nothing after
    /// it.
    fn write_key<O: Options>(value: &T, w: &mut Writer<'_, O>);
}

/// Field-by-field reading for a struct.
pub trait ReadObject<'de>: Keys + Sized {
    /// Parse the value for field `index`.
    ///
    /// `key` is the member's key, already delimited by its length prefix, and
    /// `index` is only the candidate the hash proposed: the implementation
    /// must confirm `key` against its own before reading anything.
    ///
    /// Returns `false` if the key did not match, leaving the reader positioned
    /// on the value so the caller can treat the member as unknown: under
    /// [`Options::ERROR_ON_UNKNOWN_KEYS`] that is an
    /// [`ErrorCode::UnknownKey`](crate::ErrorCode::UnknownKey), and otherwise
    /// the member is stepped over.
    ///
    /// Returning `true` is also what records the field as filled, which is
    /// what [`Options::ERROR_ON_MISSING_KEYS`] checks the object against.
    fn read_field<O: Options>(
        &mut self,
        index: usize,
        key: &[u8],
        r: &mut Reader<'de, O>,
    ) -> PResult<bool>;
}

/// Field-by-field writing for a struct.
pub trait WriteObject: Keys {
    /// Write every member as `SIZE | KEY | VALUE`.
    ///
    /// No separators and no trailing anything: the object's header already
    /// stated how many members follow, which is why [`count_fields`] has to
    /// agree with this exactly.
    ///
    /// [`count_fields`]: WriteObject::count_fields
    fn write_fields<O: Options>(&self, w: &mut Writer<'_, O>);

    /// How many members [`write_fields`](WriteObject::write_fields) will write
    /// under this policy.
    ///
    /// BEVE states an object's member count before its members, so the count
    /// has to be known first. Without [`Options::SKIP_NULL`] it is `KEYS.len()`
    /// and folds to a literal; with it, members can drop out and the answer
    /// depends on the value.
    ///
    /// There is deliberately no default body. A count that disagrees with what
    /// `write_fields` writes does not produce a document a reader rejects: it
    /// produces one where the reader takes the next value's bytes for a member,
    /// or stops short and calls the rest trailing content. That is the failure
    /// this format punishes hardest, so the trait asks rather than assumes.
    /// [`object!`](crate::object) generates both halves together; a hand-written
    /// implementation that writes every field unconditionally returns
    /// `Self::KEYS.len()`.
    ///
    /// The count is checked against what was written in a debug build, and the
    /// check counts members that went through [`Writer::member`] or
    /// [`Writer::member_with`]. Unlike the JSON side, where writing a member
    /// some other way is allowed and occasionally wanted, a BEVE member has to
    /// go through one of those two for the count to be checkable at all: an
    /// implementation that writes the key bytes itself will trip the assertion
    /// even on a correct document.
    ///
    /// [`Writer::member`]: crate::beve::Writer::member
    fn count_fields<O: Options>(&self) -> usize;
}

/// Element-by-element reading for a struct written as a BEVE array.
///
/// The positional counterpart of [`ReadObject`]. Any array form is accepted,
/// generic or typed, because the driver hands out one value per element either
/// way: a struct of three `f64`s reads back from the contiguous block another
/// implementation would have written for a `[f64; 3]`.
pub trait ReadArray<'de>: Elements + Sized {
    /// Read the value at position `index`.
    ///
    /// `index` counts from zero and is not bounded by [`Elements::LEN`]: an
    /// array longer than the struct reaches here with an index past the last
    /// field, which is an
    /// [`ErrorCode::ArrayLengthMismatch`](crate::ErrorCode::ArrayLengthMismatch).
    fn read_element<O: Options>(&mut self, index: usize, r: &mut Reader<'de, O>) -> PResult<()>;
}

/// Element-by-element writing for a struct written as a BEVE array.
pub trait WriteArray: Elements {
    /// Write every element, one value after another.
    ///
    /// No separators and no trailing anything, for the same reason
    /// [`WriteObject::write_fields`] needs none: the array header already
    /// stated how many follow, and that count is [`Elements::LEN`], known at
    /// compile time.
    fn write_elements<O: Options>(&self, w: &mut Writer<'_, O>);

    /// The array header this struct's elements are stored under, or `None` to
    /// write a generic array.
    ///
    /// The same constant [`Write::ARRAY`] carries for a sequence, and it means
    /// the same thing: `Some` replaces a header per element with one header
    /// for the lot. A struct only has one when every field is the same type,
    /// which is what declaring it with an element type asserts.
    #[doc(hidden)]
    const ARRAY: Option<&'static [u8]> = None;

    /// Append every element as the payload of a typed array whose header and
    /// count the caller has already written.
    ///
    /// Only reached when [`WriteArray::ARRAY`] is `Some`, and it is a
    /// constant, so the call folds away entirely for a struct that has no
    /// typed array.
    #[doc(hidden)]
    fn write_payload<O: Options>(&self, w: &mut Writer<'_, O>) {
        let _ = w;
        unreachable!("structio: a typed array without a payload writer")
    }
}

/// Variant-by-variant reading for an enum.
///
/// The counterpart of [`ReadObject`] for a type declared with
/// [`unit_enum!`](crate::unit_enum) or [`tagged_enum!`](crate::tagged_enum),
/// and the mirror of [`json::ReadEnum`](crate::json::ReadEnum). The wire forms
/// are the same two: a variant carrying nothing is a string, and a variant
/// carrying a value is an object of one member keyed by the name.
///
/// The name arrives already delimited, by its length prefix, so unlike the
/// JSON side there is nothing to walk to a closing quote. `index` is still
/// only the candidate the hash proposed, so both methods must confirm `name`
/// against their own before doing anything else.
pub trait ReadEnum<'de>: Variants + Sized {
    /// Take variant `index` written as a bare name.
    ///
    /// No reader is passed, because a name carries nothing after it and there
    /// is nothing left to read. Returns `false` if `name` did not match, which
    /// the caller reports as
    /// [`ErrorCode::UnknownVariant`](crate::ErrorCode::UnknownVariant). A
    /// variant that carries a value has no bare form and answers with
    /// [`ErrorCode::ExpectedObject`](crate::ErrorCode::ExpectedObject)
    /// instead: its name was recognized, and what is missing is the value
    /// under it.
    fn read_name(&mut self, index: usize, name: &[u8]) -> PResult<bool>;

    /// Take variant `index` written as the single member of an object, the
    /// reader positioned on that member's value.
    ///
    /// A variant that carries nothing accepts `null` here, so a producer that
    /// always writes the object form still round-trips.
    ///
    /// Returns `false` if `name` did not match, with the same meaning it has
    /// for [`read_name`](Self::read_name).
    fn read_payload<O: Options>(
        &mut self,
        index: usize,
        name: &[u8],
        r: &mut Reader<'de, O>,
    ) -> PResult<bool>;
}

/// Convenience bound for generic containers: readable from any BEVE input, and
/// writable.
///
/// Prefer [`crate::ReadWrite`], which also covers JSON, unless the type is
/// deliberately BEVE only.
pub trait ReadWrite: for<'de> Read<'de> + Write {}
impl<T> ReadWrite for T where T: for<'de> Read<'de> + Write {}
