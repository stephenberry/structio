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
    /// The header and the count are already consumed, so `n` and `elem` are
    /// what they said and the reader is positioned on the payload. `elem` is
    /// the *element's* header, not the array's, and comparing it against the
    /// one this type carries is the half of the contract the caller cannot
    /// check: a stored `f32` and a stored `f64` reach here alike.
    ///
    /// [`Reader::read_block`] is the copy itself, for a type whose memory is
    /// already the payload. An implementation with a conversion to do has
    /// nothing to gain here and should leave this alone.
    ///
    /// Implementations must consume from `r` only when they return `true`,
    /// and must not turn one of its errors into `Ok(false)`. The caller puts
    /// the cursor back, so an implementation that reads and then declines is
    /// corrected rather than believed; what it does not put back is the
    /// implied element header and the depth, which only a walk abandoned
    /// part-way leaves disturbed.
    ///
    /// [`Reader::read_block`]: crate::beve::Reader::read_block
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
    ///
    /// Leaving it `None` is never wrong: a run of values written one header
    /// each is a document every reader accepts, and a larger one. Setting it
    /// obliges [`write_payload`](Write::write_payload) to emit exactly what
    /// the named array's payload is, which for a run of numbers is the
    /// little-endian bytes of each element and nothing between them.
    const ARRAY: Option<&'static [u8]> = None;

    /// Append `items` as the payload of a typed array whose header and count
    /// the caller has already written.
    ///
    /// Only reached when [`Write::ARRAY`] is `Some`, and it is a constant, so
    /// the call folds away entirely for the types that have no typed array.
    ///
    /// The preamble is done: the header bytes, the count, and under
    /// [`Writer::aligned`](crate::beve::Writer::aligned) the padding that
    /// lands the payload on an address the element width divides. All that is
    /// left is `items.len()` elements' worth of bytes, appended with nothing
    /// between them. [`Writer::write_block`] is that copy for a type whose
    /// memory is already the payload.
    ///
    /// # Panics
    ///
    /// The default body does, and says so: a type that names no array has no
    /// payload to append, and the crate reaches here only through the `Some`
    /// arm of the match on [`ARRAY`](Write::ARRAY). Overriding one without the
    /// other is the mistake it exists to catch.
    ///
    /// [`Writer::write_block`]: crate::beve::Writer::write_block
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

    /// Fill `out` from the payload of a typed array of `n` elements whose
    /// element header is `elem`, or return `false` to decline.
    ///
    /// [`Read::read_bulk`] moved onto the adapter, under the same contract in
    /// full: the header and count are consumed, `elem` is the element's own
    /// header and has to be checked, nothing may be taken from `r` except on
    /// the way to returning `true`, and one of its errors may not be turned
    /// into `Ok(false)`.
    ///
    /// It is the reading half of [`WriteAs::ARRAY`], and it exists for the
    /// same reason. Without it an adapted sequence would give up the bulk path
    /// even where the adapter changes nothing, which is why
    /// [`Same`](crate::Same) forwards this exactly as it forwards `ARRAY`.
    ///
    /// Only [`Vec`] reaches here. It is the one sequence whose storage is a
    /// block already, so it is the one a block can be copied into.
    #[inline]
    fn read_bulk<O: Options>(
        _out: &mut Vec<T>,
        _n: usize,
        _elem: u8,
        _r: &mut Reader<'de, O>,
    ) -> PResult<bool> {
        Ok(false)
    }
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
    ///
    /// This is how a scalar from a crate you do not own reaches the typed
    /// array. `T` needs no impl of its own: the adapter names the array here
    /// and fills it in [`write_payload`](WriteAs::write_payload), and both are
    /// reached through [`Writer::write_slice_with`], which dispatches on this
    /// constant rather than on [`Write::ARRAY`].
    ///
    /// [`Writer::write_slice_with`]: crate::beve::Writer::write_slice_with
    const ARRAY: Option<&'static [u8]> = None;

    /// Append `items` as the payload of a typed array whose header and count
    /// the caller has already written.
    ///
    /// Only reached when [`WriteAs::ARRAY`] is `Some`, and it is a constant, so
    /// the call folds away entirely for an adapter that has no typed array.
    ///
    /// The same contract as [`Write::write_payload`], against the array this
    /// adapter named rather than the one the type would have, and the same
    /// panic in the default body for an adapter that names none.
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
    const ARRAY: Option<&'static [u8]> = None;

    /// Append every element as the payload of a typed array whose header and
    /// count the caller has already written.
    ///
    /// Only reached when [`WriteArray::ARRAY`] is `Some`, and it is a
    /// constant, so the call folds away entirely for a struct that has no
    /// typed array.
    ///
    /// The same contract as [`Write::write_payload`]: the preamble is done,
    /// what is left is [`LEN`](Elements::LEN) elements' worth of bytes, and
    /// the default body panics for a struct that names no array.
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

/// Variant-by-variant reading for an internally tagged enum.
///
/// The counterpart of [`ReadEnum`] for a type declared with
/// [`internally_tagged_enum!`](crate::internally_tagged_enum), and the mirror
/// of [`json::ReadInternallyTagged`](crate::json::ReadInternallyTagged). One
/// method rather than two, there being one wire form: an object whose first
/// member is the tag, and whose remaining members are the variant's own.
pub trait ReadInternallyTagged<'de>: Variants + Sized {
    /// The key that carries the variant name, and which every document must
    /// put first.
    const TAG: &'static str;

    /// Take variant `index`, the reader positioned on the members that follow
    /// the tag.
    ///
    /// `name` is the tag's value, already delimited by its length prefix.
    /// `remaining` is how many members of the object are left after the tag,
    /// which is what says where the object ends: BEVE counts its members up
    /// front rather than closing them with a brace.
    ///
    /// The implementation reads those members, a variant carrying a value into
    /// its payload and one carrying nothing over them. Returns `false` if
    /// `name` did not match, which the caller reports as
    /// [`ErrorCode::UnknownVariant`](crate::ErrorCode::UnknownVariant).
    ///
    /// `open` is the offset of the object's header byte, carried for
    /// [`json::ReadInternallyTagged::read_variant`]'s reason: a
    /// [`MissingKey`](crate::ErrorCode::MissingKey) names the object it is
    /// missing from, and the cursor is past it by now.
    ///
    /// [`json::ReadInternallyTagged::read_variant`]: crate::json::ReadInternallyTagged::read_variant
    fn read_variant<O: Options>(
        &mut self,
        index: usize,
        name: &[u8],
        r: &mut Reader<'de, O>,
        remaining: usize,
        open: usize,
    ) -> PResult<bool>;
}

/// Convenience bound for generic containers: readable from any BEVE input, and
/// writable.
///
/// Prefer [`crate::ReadWrite`], which also covers JSON, unless the type is
/// deliberately BEVE only.
pub trait ReadWrite: for<'de> Read<'de> + Write {}
impl<T> ReadWrite for T where T: for<'de> Read<'de> + Write {}
