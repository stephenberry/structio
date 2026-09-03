//! [`Read`] and [`Write`] for the standard library's types.
//!
//! The type set is the same as the JSON side's, and reading follows the same
//! rule: always *into* an existing value, so containers reuse the storage they
//! already hold.
//!
//! What is different is what a sequence becomes. A `Vec<f64>` is not a list of
//! separately tagged numbers here; it is one header, one count, and the bytes
//! of the slice, which on a little-endian host is the slice's own memory. That
//! is BEVE's reason to exist, and it is why [`Write::ARRAY`] appears on the
//! element type rather than on the container.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::hash::{BuildHasher, Hash};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;

use crate::beve::header;
use crate::beve::reader::{Key, Reader, cautious};
use crate::beve::traits::{
    Read, ReadArray, ReadAs, ReadKeyAs, Write, WriteArray, WriteAs, WriteKeyAs,
};
use crate::beve::writer::Writer;
use crate::error::{ErrorCode, PResult};
use crate::options::Options;
use crate::traits::Same;

// ---------------------------------------------------------------------------
// Object keys
// ---------------------------------------------------------------------------

/// A type usable as a BEVE object key.
///
/// Unlike JSON, BEVE keys are not always strings: an object declares its key
/// type in its header, so a `HashMap<u32, _>` stores real integers and needs
/// no stringification round trip.
pub trait ToBeveKey {
    /// The object header an object keyed by this type carries.
    const OBJECT: u8;

    /// Write one key, with no header of its own.
    fn write_key<O: Options>(&self, w: &mut Writer<'_, O>);
}

/// The reading half of [`ToBeveKey`].
pub trait FromBeveKey: Sized {
    fn from_key(key: Key<'_>) -> PResult<Self>;
}

impl FromBeveKey for String {
    #[inline]
    fn from_key(key: Key<'_>) -> PResult<Self> {
        match key {
            Key::Str(s) => Ok(s.to_owned()),
            _ => Err(ErrorCode::UnsupportedKeyType),
        }
    }
}

macro_rules! impl_str_key {
    ($([$($gen:tt)*] $ty:ty),* $(,)?) => {$(
        impl<$($gen)*> ToBeveKey for $ty {
            const OBJECT: u8 = header::OBJECT;
            #[inline]
            fn write_key<O: Options>(&self, w: &mut Writer<'_, O>) {
                w.write_str_body(self);
            }
        }
    )*}
}
impl_str_key!([] String, [] str, [] &str, ['a] Cow<'a, str>);

impl FromBeveKey for char {
    #[inline]
    fn from_key(key: Key<'_>) -> PResult<Self> {
        let Key::Str(s) = key else {
            return Err(ErrorCode::UnsupportedKeyType);
        };
        let mut it = s.chars();
        match (it.next(), it.next()) {
            (Some(c), None) => Ok(c),
            _ => Err(ErrorCode::ExpectedSingleChar),
        }
    }
}

impl ToBeveKey for char {
    const OBJECT: u8 = header::OBJECT;
    #[inline]
    fn write_key<O: Options>(&self, w: &mut Writer<'_, O>) {
        let mut buf = [0u8; 4];
        w.write_str_body(self.encode_utf8(&mut buf));
    }
}

/// An integer key is stored as an integer, at its own width.
///
/// A string key is still accepted on the way in, so a document produced from
/// JSON, where the key had to be quoted, still reads back into a numeric map.
macro_rules! impl_int_key {
    ($cat:expr; $($t:ty),*) => {$(
        impl FromBeveKey for $t {
            #[inline]
            fn from_key(key: Key<'_>) -> PResult<Self> {
                match key {
                    Key::Unsigned(v) => {
                        <$t>::try_from(v).map_err(|_| ErrorCode::NumberOutOfRange)
                    }
                    Key::Signed(v) => {
                        <$t>::try_from(v).map_err(|_| ErrorCode::NumberOutOfRange)
                    }
                    Key::Str(s) => s.parse::<$t>().map_err(|_| ErrorCode::InvalidNumber),
                }
            }
        }
        impl ToBeveKey for $t {
            const OBJECT: u8 =
                header::header(header::TY_OBJECT, $cat, header::code_for(size_of::<$t>()));
            #[inline]
            fn write_key<O: Options>(&self, w: &mut Writer<'_, O>) {
                w.raw(&self.to_le_bytes());
            }
        }
    )*}
}
impl_int_key!(header::CAT_SIGNED; i8, i16, i32, i64, isize, i128);
impl_int_key!(header::CAT_UNSIGNED; u8, u16, u32, u64, usize, u128);

// ---------------------------------------------------------------------------
// Scalars
// ---------------------------------------------------------------------------

impl<'de> Read<'de> for bool {
    #[inline]
    fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
        *self = r.read_bool()?;
        Ok(())
    }
}

impl Write for bool {
    const ARRAY: Option<&'static [u8]> = Some(&[header::BOOL_ARRAY]);

    #[inline]
    fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
        w.write_bool(*self);
    }

    /// Booleans pack one per bit, low bit first, with the tail of the last
    /// byte left zero.
    fn write_payload<O: Options>(items: &[bool], w: &mut Writer<'_, O>) {
        let mut byte = 0u8;
        for (i, &b) in items.iter().enumerate() {
            byte |= (b as u8) << (i & 7);
            if i & 7 == 7 {
                w.push(byte);
                byte = 0;
            }
        }
        if items.len() & 7 != 0 {
            w.push(byte);
        }
    }
}

/// The unit type maps to `null`, so it can stand in for a field that carries
/// no information.
impl<'de> Read<'de> for () {
    #[inline]
    fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
        if r.try_null()? {
            Ok(())
        } else {
            Err(ErrorCode::ExpectedNull)
        }
    }
}

impl Write for () {
    #[inline]
    fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
        w.write_null();
    }

    /// The unit writes null unconditionally, so it is always the absent one.
    #[inline]
    fn is_null(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// The bulk number paths
// ---------------------------------------------------------------------------
//
// A typed array of numbers is, on a little-endian host, already the in-memory
// form of the corresponding slice, so both directions are one copy. Integers
// and floats want identical code for it, and it is the only place in the crate
// that reinterprets memory, so `NumericBytes` below holds the whole of that
// argument rather than each numeric macro restating it. The two copies
// themselves are `Reader::read_block` and `Writer::write_block`, which sit
// with the cursor and the buffer they move bytes through and are public for
// the same reason the traits' bulk hooks are: an adapter over a foreign scalar
// has to be able to reach them.

/// Types whose little-endian in-memory form *is* a BEVE typed array's payload.
///
/// The bound on everything in this crate that reinterprets memory:
/// [`Reader::read_block`] and [`Writer::write_block`], which copy a whole
/// payload in each direction, and [`Reader::try_slice`], which hands one back
/// as a slice pointing into the document without copying at all.
///
/// The crate implements it for every number BEVE can name, and for
/// [`Complex`](crate::Complex) of each. It is implementable from outside for
/// the case those cannot cover: a scalar from a crate you do not own, whose
/// memory is already a payload, reached through an
/// [adapter](crate::beve::ReadAs) because the orphan rule keeps [`Read`] and
/// [`Write`] off it. That is the whole reason it is not sealed, and it is why
/// it is `unsafe`: nothing here is checked at a use site, and the obligations
/// below are the reader's and the writer's only grounds for reinterpreting the
/// bytes.
///
/// # Safety
///
/// Implementing this asserts four things about `Self`:
///
/// - it occupies `size_of::<Self>()` initialized bytes with no padding, so
///   reading them as bytes exposes nothing uninitialized;
/// - every bit pattern of that size is a valid value, so bytes off the wire
///   can become one without being checked;
/// - on a little-endian host those bytes are exactly what BEVE stores for one
///   element of [`ELEMENT`](NumericBytes::ELEMENT), in exactly that order;
/// - one such element is `size_of::<Self>()` bytes of payload, so a block of
///   `n` of them is exactly `n * size_of::<Self>()` bytes.
///
/// The fourth is the one a compiler can check, and it is checked: an impl
/// whose declared element is a width other than its own is refused where the
/// block helpers are instantiated. The first three are yours to hold.
/// `#[repr(transparent)]` over a primitive satisfies all four by construction
/// and is the shape this expects.
///
/// Nothing here says a *document* holds this type. That stays a runtime test
/// against [`ELEMENT`](NumericBytes::ELEMENT), and the helpers make the caller
/// do it: see [`Read::read_bulk`].
///
/// [`Reader::read_block`]: crate::beve::Reader::read_block
/// [`Writer::write_block`]: crate::beve::Writer::write_block
pub unsafe trait NumericBytes: Copy {
    /// The header one of these carries when it stands alone, which is the
    /// header a typed array of them implies for its elements.
    ///
    /// What a stored array's element type is compared against, and so what
    /// decides whether its payload is already this type's memory or has to be
    /// converted element by element. A newtype over a primitive forwards the
    /// primitive's:
    ///
    /// ```
    /// use structio::beve::NumericBytes;
    ///
    /// #[derive(Clone, Copy)]
    /// #[repr(transparent)]
    /// struct Celsius(f64);
    ///
    /// // SAFETY: `repr(transparent)` over `f64`, which satisfies all four
    /// // clauses, and the declared element is `f64`'s own.
    /// unsafe impl NumericBytes for Celsius {
    ///     const ELEMENT: u8 = <f64 as NumericBytes>::ELEMENT;
    /// }
    /// ```
    const ELEMENT: u8;
}

/// The width arithmetic behind a block, and the one clause of the
/// [`NumericBytes`] contract a compiler can check.
///
/// A constant of a generic type, so it is evaluated once per element type that
/// reaches a block helper and a type that disagrees with its own declared
/// element is refused when the crate using it is built. The impls in this file
/// are checked where they are written, by `numeric_bytes!`; this is the same
/// check for one written somewhere else.
pub(crate) struct Block<T>(PhantomData<fn(T)>);

impl<T: NumericBytes> Block<T> {
    /// Bytes one element occupies, which by the contract is `size_of::<T>()`.
    ///
    /// Used in place of `size_of::<T>()` at every site that measures a
    /// payload, so the check is load bearing rather than a dangling assertion
    /// somebody could delete without noticing.
    pub(crate) const WIDTH: usize = match header::element_width(T::ELEMENT) {
        Some(width) => {
            assert!(
                width == size_of::<T>(),
                "structio: a `NumericBytes` type whose declared element is not its own width"
            );
            width
        }
        None => panic!("structio: a `NumericBytes` type whose declared element has no width"),
    };
}

/// Every primitive of a fixed width: no padding, every bit pattern a value,
/// and the little-endian bytes are the wire form by construction.
///
/// The width codes are given rather than derived because the float ones do not
/// follow `1 << count`; see [`header::byte_width`]. This list is where each
/// type's wire class is settled, and the macros below take it from here rather
/// than restating it.
macro_rules! numeric_bytes {
    ($($t:ty, $cat:expr, $code:expr);* $(;)?) => {$(
        // The declared class is the type's own width, which is the one clause
        // of the marker a compiler can check: it is what makes a payload of
        // `n` elements exactly `n * size_of::<Self>()` bytes. `Block::WIDTH`
        // makes the same check for every impl, this one included, but only
        // where a block helper is instantiated; here it lands on the
        // declaration, which is where a wrong one would be written.
        const _: () = match header::byte_width($cat, $code) {
            Some(width) => assert!(width == size_of::<$t>()),
            None => panic!("structio: a numeric type with no width in BEVE"),
        };

        unsafe impl NumericBytes for $t {
            const ELEMENT: u8 = header::number($cat, $code);
        }
    )*}
}

numeric_bytes! {
    f32, header::CAT_FLOAT, 2;
    f64, header::CAT_FLOAT, 3;
    i8, header::CAT_SIGNED, header::code_for(1);
    i16, header::CAT_SIGNED, header::code_for(2);
    i32, header::CAT_SIGNED, header::code_for(4);
    i64, header::CAT_SIGNED, header::code_for(8);
    i128, header::CAT_SIGNED, header::code_for(16);
    isize, header::CAT_SIGNED, header::code_for(size_of::<isize>());
    u8, header::CAT_UNSIGNED, header::code_for(1);
    u16, header::CAT_UNSIGNED, header::code_for(2);
    u32, header::CAT_UNSIGNED, header::code_for(4);
    u64, header::CAT_UNSIGNED, header::code_for(8);
    u128, header::CAT_UNSIGNED, header::code_for(16);
    usize, header::CAT_UNSIGNED, header::code_for(size_of::<usize>());
}

/// Integers are stored at their declared width, and read from any width that
/// the value fits in. See the [reader](crate::beve::Reader) docs.
macro_rules! impl_int {
    ($cat:expr, $read:ident; $($t:ty),*) => {$(
        impl<'de> Read<'de> for $t {
            #[inline]
            fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
                let v = r.$read()?;
                *self = <$t>::try_from(v).map_err(|_| ErrorCode::NumberOutOfRange)?;
                Ok(())
            }

            fn read_bulk<O: Options>(
                out: &mut Vec<$t>,
                n: usize,
                elem: u8,
                r: &mut Reader<'de, O>,
            ) -> PResult<bool> {
                if elem != <$t as NumericBytes>::ELEMENT || cfg!(target_endian = "big") {
                    return Ok(false);
                }
                // The tag test above is the half of the contract the bound
                // does not cover: it establishes that the stored element type
                // is this one.
                r.read_block(out, n)?;
                Ok(true)
            }
        }

        impl Write for $t {
            const ARRAY: Option<&'static [u8]> =
                Some(&[header::array_of($cat, header::code_for(size_of::<$t>()))]);

            #[inline]
            fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
                const TAG: u8 = header::number($cat, header::code_for(size_of::<$t>()));
                w.write_number(TAG, self.to_le_bytes());
            }

            fn write_payload<O: Options>(items: &[$t], w: &mut Writer<'_, O>) {
                if cfg!(target_endian = "little") {
                    w.write_block(items)
                } else {
                    for v in items {
                        w.raw(&v.to_le_bytes());
                    }
                }
            }
        }
    )*}
}
impl_int!(header::CAT_UNSIGNED, read_u64; u8, u16, u32, u64, usize);
impl_int!(header::CAT_SIGNED, read_i64; i8, i16, i32, i64, isize);
impl_int!(header::CAT_UNSIGNED, read_u128; u128);
impl_int!(header::CAT_SIGNED, read_i128; i128);

/// Floats follow the same shape, minus the range check: every stored width
/// widens into `f64` exactly, and `f32` is the one narrowing conversion.
macro_rules! impl_float {
    ($($t:ty, $read:ident, $code:expr);* $(;)?) => {$(
        impl<'de> Read<'de> for $t {
            #[inline]
            fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
                *self = r.$read()?;
                Ok(())
            }

            fn read_bulk<O: Options>(
                out: &mut Vec<$t>,
                n: usize,
                elem: u8,
                r: &mut Reader<'de, O>,
            ) -> PResult<bool> {
                if elem != <$t as NumericBytes>::ELEMENT || cfg!(target_endian = "big") {
                    return Ok(false);
                }
                // As in `impl_int!`: the tag test is what the bound leaves to
                // the caller.
                r.read_block(out, n)?;
                Ok(true)
            }
        }

        impl Write for $t {
            const ARRAY: Option<&'static [u8]> =
                Some(&[header::array_of(header::CAT_FLOAT, $code)]);

            #[inline]
            fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
                const TAG: u8 = header::number(header::CAT_FLOAT, $code);
                w.write_number(TAG, self.to_le_bytes());
            }

            fn write_payload<O: Options>(items: &[$t], w: &mut Writer<'_, O>) {
                if cfg!(target_endian = "little") {
                    w.write_block(items)
                } else {
                    for v in items {
                        w.raw(&v.to_le_bytes());
                    }
                }
            }
        }
    )*}
}
impl_float!(f32, read_f32, 2; f64, read_f64, 3);

// ---------------------------------------------------------------------------
// Strings
// ---------------------------------------------------------------------------

impl<'de> Read<'de> for String {
    #[inline]
    fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
        r.read_string_into(self)
    }
}

/// String array elements carry no header of their own, only their size, which
/// is what [`Writer::write_str_body`] emits.
macro_rules! impl_write_str {
    ($([$($gen:tt)*] $ty:ty),* $(,)?) => {$(
        impl<$($gen)*> Write for $ty {
            const ARRAY: Option<&'static [u8]> = Some(&[header::STRING_ARRAY]);

            #[inline]
            fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
                w.write_str(self);
            }

            fn write_payload<O: Options>(items: &[Self], w: &mut Writer<'_, O>) where Self: Sized {
                for s in items {
                    w.write_str_body(s);
                }
            }
        }
    )*}
}
impl_write_str!([] String, ['a] Cow<'a, str>);

/// `str` is unsized, so it can never be a typed array's element type and keeps
/// the default layout.
impl Write for str {
    #[inline]
    fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
        w.write_str(self);
    }
}

/// Borrow a string straight out of the input, with no copy at all.
///
/// BEVE stores strings verbatim, so unlike JSON this never has to fail: there
/// is no escaped form that would need rebuilding.
impl<'de> Read<'de> for &'de str {
    #[inline]
    fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
        *self = r.read_str()?;
        Ok(())
    }
}

impl<'de> Read<'de> for Cow<'de, str> {
    #[inline]
    fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
        *self = Cow::Borrowed(r.read_str()?);
        Ok(())
    }
}

/// Borrow a byte array straight out of the input.
///
/// The counterpart of `&'de str` for binary payloads: a `Vec<u8>` field costs
/// one copy, this costs none.
impl<'de> Read<'de> for &'de [u8] {
    #[inline]
    fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
        *self = r.read_bytes()?;
        Ok(())
    }
}

impl<'de> Read<'de> for char {
    #[inline]
    fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
        let s = r.read_str()?;
        let mut it = s.chars();
        match (it.next(), it.next()) {
            (Some(c), None) => {
                *self = c;
                Ok(())
            }
            _ => Err(ErrorCode::ExpectedSingleChar),
        }
    }
}

impl Write for char {
    const ARRAY: Option<&'static [u8]> = Some(&[header::STRING_ARRAY]);

    #[inline]
    fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
        let mut buf = [0u8; 4];
        w.write_str(self.encode_utf8(&mut buf));
    }

    fn write_payload<O: Options>(items: &[char], w: &mut Writer<'_, O>) {
        let mut buf = [0u8; 4];
        for c in items {
            w.write_str_body(c.encode_utf8(&mut buf));
        }
    }
}

// ---------------------------------------------------------------------------
// Wrappers
// ---------------------------------------------------------------------------

impl<'de, T> Read<'de> for Option<T>
where
    T: Read<'de> + Default,
{
    #[inline]
    fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
        if r.try_null()? {
            *self = None;
            return Ok(());
        }
        match self {
            // Read over the existing payload so its allocations survive.
            Some(v) => v.read(r),
            None => {
                let mut v = T::default();
                v.read(r)?;
                *self = Some(v);
                Ok(())
            }
        }
    }
}

impl<T: Write> Write for Option<T> {
    #[inline]
    fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
        match self {
            Some(v) => v.write(w),
            None => w.write_null(),
        }
    }

    /// `None` is the absence [`Options::SKIP_NULL`] is named for. A `Some`
    /// defers to what it holds, so `Some(())` is absent for the same reason a
    /// bare `()` is.
    #[inline]
    fn is_null(&self) -> bool {
        match self {
            Some(v) => v.is_null(),
            None => true,
        }
    }
}

impl<'de, T: Read<'de>> Read<'de> for Box<T> {
    #[inline]
    fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
        (**self).read(r)
    }
}

/// Reference-counted payloads are read in place when this handle is the only
/// one, matching `Box`. A shared payload cannot be touched, so it is replaced.
macro_rules! impl_read_shared {
    ($($ty:ident),* $(,)?) => {$(
        impl<'de, T: Read<'de> + Default> Read<'de> for $ty<T> {
            #[inline]
            fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
                if let Some(v) = $ty::get_mut(self) {
                    return v.read(r);
                }
                let mut v = T::default();
                v.read(r)?;
                *self = $ty::new(v);
                Ok(())
            }
        }
    )*}
}
impl_read_shared!(Rc, Arc);

/// A wrapper writes exactly what it points at.
///
/// It keeps the default `ARRAY` of `None`, so a `Vec<&f64>` is a generic
/// array rather than a typed one: a typed array's payload is one contiguous
/// block, and a slice of references is not one. Readers take either form, so
/// the choice costs compactness and nothing else.
macro_rules! impl_write_deref {
    ($($ty:ty),* $(,)?) => {$(
        impl<T: Write + ?Sized> Write for $ty {
            #[inline]
            fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
                (**self).write(w);
            }

            #[inline]
            fn is_null(&self) -> bool {
                (**self).is_null()
            }
        }
    )*}
}
impl_write_deref!(Box<T>, Rc<T>, Arc<T>, &T);

// ---------------------------------------------------------------------------
// Sequences
// ---------------------------------------------------------------------------

/// Read over elements that are already here before growing, so a sequence of
/// `String` reuses every buffer it is holding.
impl<'de, T: Read<'de> + Default> Read<'de> for Vec<T> {
    fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
        // The whole array in one copy, when the stored element type is this
        // one. Declines without consuming anything otherwise.
        if r.try_bulk(self)? {
            return Ok(());
        }
        let held = self.len();
        // A reborrow, so that what the closures take is given back for the
        // truncate.
        let out = &mut *self;
        let n = r.read_seq_counted(|n| {
            out.reserve(cautious::<T>(n).saturating_sub(held));
            move |r, i| {
                if i < held {
                    out[i].read(r)
                } else {
                    let mut v = T::default();
                    v.read(r)?;
                    out.push(v);
                    Ok(())
                }
            }
        })?;
        self.truncate(n);
        Ok(())
    }
}

/// Borrow a numeric block straight out of the input when the document allows
/// it, and copy when it does not.
///
/// The sequence counterpart of `Cow<'de, str>`, and unlike it this cannot
/// always borrow: [`Reader::try_slice`] says what a borrow needs. That is also
/// why there is no `Read` for `&'de [f64]` itself, where there is one for
/// `&'de [u8]`: a field that must borrow would make a program's correctness
/// depend on the address its input happened to be allocated at, and a byte is
/// the one width with no address to satisfy.
impl<'de, T> Read<'de> for Cow<'de, [T]>
where
    T: NumericBytes + Read<'de> + Default,
{
    fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
        if let Some(block) = r.try_slice::<T>() {
            *self = Cow::Borrowed(block);
            return Ok(());
        }
        match self {
            // Read over what is owned here, so a `Cow` that has been read into
            // before keeps its allocation exactly as a `Vec` does.
            Cow::Owned(v) => v.read(r),
            Cow::Borrowed(_) => {
                let mut v = Vec::new();
                v.read(r)?;
                *self = Cow::Owned(v);
                Ok(())
            }
        }
    }
}

/// Borrowed or owned, it is written as the slice it derefs to.
impl<T: Write + Clone> Write for Cow<'_, [T]> {
    #[inline]
    fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
        w.write_slice(self);
    }
}

impl<'de, T: Read<'de> + Default> Read<'de> for VecDeque<T> {
    fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
        let held = self.len();
        // A reborrow, so that what the closures take is given back for the
        // truncate.
        let out = &mut *self;
        let n = r.read_seq_counted(|n| {
            out.reserve(cautious::<T>(n).saturating_sub(held));
            move |r, i| {
                if i < held {
                    out[i].read(r)
                } else {
                    let mut v = T::default();
                    v.read(r)?;
                    out.push_back(v);
                    Ok(())
                }
            }
        })?;
        self.truncate(n);
        Ok(())
    }
}

impl<'de, T: Read<'de>, const N: usize> Read<'de> for [T; N] {
    fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
        let n = r.read_seq(|r, i| {
            if i >= N {
                return Err(ErrorCode::ArrayLengthMismatch);
            }
            self[i].read(r)
        })?;
        if n != N {
            return Err(ErrorCode::ArrayLengthMismatch);
        }
        Ok(())
    }
}

/// A set has no positional storage to reuse, so it is emptied and refilled.
/// A hashed one reserves on the count; a tree has nothing to reserve.
macro_rules! impl_read_set {
    ($([$($gen:tt)*] $ty:ty $(, reserve $reserve:ident)?),* $(,)?) => {$(
        impl<'de, T: Read<'de> + Default $($gen)*> Read<'de> for $ty {
            fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
                self.clear();
                r.read_seq_counted(|_n| {
                    $( self.$reserve(cautious::<T>(_n)); )?
                    move |r, _| {
                        let mut v = T::default();
                        v.read(r)?;
                        self.insert(v);
                        Ok(())
                    }
                })?;
                Ok(())
            }
        }
    )*}
}
impl_read_set!(
    [+ Eq + Hash, S: BuildHasher + Default] HashSet<T, S>, reserve reserve,
    [+ Ord] BTreeSet<T>,
);

/// Contiguous sequences become typed arrays when their element type has one.
macro_rules! impl_write_slice {
    ($([$($gen:tt)*] $ty:ty),* $(,)?) => {$(
        impl<T: Write $($gen)*> Write for $ty {
            #[inline]
            fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
                w.write_slice(self);
            }
        }
    )*}
}
impl_write_slice!([] Vec<T>, [] [T], [, const N: usize] [T; N]);

/// Sequences with no single backing slice, which is what a typed array's
/// payload needs, are written as generic arrays. See [`Writer::write_iter`].
macro_rules! impl_write_iter {
    ($([$($gen:tt)*] $ty:ty),* $(,)?) => {$(
        impl<T: Write $($gen)*> Write for $ty {
            #[inline]
            fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
                w.write_iter(self.len(), self.iter());
            }
        }
    )*}
}
impl_write_iter!(
    [] VecDeque<T>,
    [, S] HashSet<T, S>,
    [] BTreeSet<T>,
);

// ---------------------------------------------------------------------------
// Maps
// ---------------------------------------------------------------------------

/// A hashed map reserves on the member count; a tree has nothing to reserve.
macro_rules! impl_map {
    ($([$($kb:tt)*] [$($rgen:tt)*] [$($wgen:tt)*] $ty:ty $(, reserve $reserve:ident)?),* $(,)?) => {$(
        impl<'de, K: FromBeveKey $($kb)*, V: Read<'de> + Default $($rgen)*> Read<'de> for $ty {
            fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
                self.clear();
                r.read_map_counted(|_n| {
                    $( self.$reserve(cautious::<(K, V)>(_n)); )?
                    move |r, key| {
                        let k = K::from_key(key)?;
                        let mut v = V::default();
                        v.read(r)?;
                        self.insert(k, v);
                        Ok(())
                    }
                })
            }
        }

        impl<K: ToBeveKey, V: Write $($wgen)*> Write for $ty {
            #[inline]
            fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
                w.write_keyed(self.len(), self.iter());
            }
        }
    )*}
}
impl_map!(
    [+ Eq + Hash] [, S: BuildHasher + Default] [, S] HashMap<K, V, S>, reserve reserve,
    [+ Ord] [] [] BTreeMap<K, V>,
);

// ---------------------------------------------------------------------------
// Tuples, as generic arrays
// ---------------------------------------------------------------------------

macro_rules! impl_tuple {
    ($n:expr; $($name:ident $idx:tt),+) => {
        impl<'de, $($name: Read<'de>),+> ReadArray<'de> for ($($name,)+) {
            #[inline]
            fn read_element<O: Options>(&mut self, index: usize, r: &mut Reader<'de, O>) -> PResult<()> {
                // A fixed-width tuple maps to a fixed-length array, so the
                // element index selects the member directly.
                $(if index == $idx { return self.$idx.read(r); })+
                Err(ErrorCode::ArrayLengthMismatch)
            }
        }

        impl<$($name: Write),+> WriteArray for ($($name,)+) {
            #[inline]
            fn write_elements<O: Options>(&self, w: &mut Writer<'_, O>) {
                $( w.element(&self.$idx); )+
            }
        }

        impl<'de, $($name: Read<'de>),+> Read<'de> for ($($name,)+) {
            fn read<O: Options>(&mut self, r: &mut Reader<'de, O>) -> PResult<()> {
                r.read_array(self)
            }
        }

        impl<$($name: Write),+> Write for ($($name,)+) {
            #[inline]
            fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
                w.write_array(self);
            }
        }
    }
}

impl_tuple!(1; A 0);
impl_tuple!(2; A 0, B 1);
impl_tuple!(3; A 0, B 1, C 2);
impl_tuple!(4; A 0, B 1, C 2, D 3);
impl_tuple!(5; A 0, B 1, C 2, D 3, E 4);
impl_tuple!(6; A 0, B 1, C 2, D 3, E 4, F 5);
impl_tuple!(7; A 0, B 1, C 2, D 3, E 4, F 5, G 6);
impl_tuple!(8; A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7);
impl_tuple!(9; A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8);
impl_tuple!(10; A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9);
impl_tuple!(11; A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10);
impl_tuple!(12; A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10, L 11);

// ---------------------------------------------------------------------------
// Adapters
// ---------------------------------------------------------------------------
//
// The mirror of the JSON side's adapter section, and of the plain impls above,
// in the same order. See `docs/schemas.md` for what an adapter is for; the
// notes that differ from the JSON side are the two below.
//
// Writing keeps the typed array. [`WriteAs::ARRAY`] is the adapter's own
// answer to [`Write::ARRAY`], so a `Vec<Same>` over a `Vec<f64>` is still one
// header and one block. Without it every adapted sequence would quietly become
// a generic array: a valid document, one byte per element larger.
//
// Reading keeps it too, through the matching hook. `ReadAs::read_bulk` is the
// adapter's answer to `Read::read_bulk`, and `Reader::try_bulk_with` offers a
// block to it exactly as `try_bulk` offers one to the type; `Same` forwards to
// the type's, so a `Vec<Same>` is the same `memcpy` the bare `Vec` is. An
// adapter that leaves the hook alone declines and reads element by element,
// which is the right answer whenever the adapter has a conversion to do.

impl<'de, T: Read<'de>> ReadAs<'de, T> for Same {
    #[inline]
    fn read<O: Options>(value: &mut T, r: &mut Reader<'de, O>) -> PResult<()> {
        value.read(r)
    }

    /// The identity on the reading side of a block, as `ARRAY` is on the
    /// writing side: whatever the type would have done with the payload,
    /// including declining it.
    #[inline]
    fn read_bulk<O: Options>(
        out: &mut Vec<T>,
        n: usize,
        elem: u8,
        r: &mut Reader<'de, O>,
    ) -> PResult<bool> {
        <T as Read<'de>>::read_bulk(out, n, elem, r)
    }
}

impl<T: Write + ?Sized> WriteAs<T> for Same {
    #[inline]
    fn write<O: Options>(value: &T, w: &mut Writer<'_, O>) {
        value.write(w);
    }

    #[inline]
    fn is_null(value: &T) -> bool {
        value.is_null()
    }

    /// The identity on the bytes as well as on the values: without this
    /// forwarding a `Vec<Same>` would drop out of its typed array.
    const ARRAY: Option<&'static [u8]> = <T as Write>::ARRAY;

    #[inline]
    fn write_payload<O: Options>(items: &[T], w: &mut Writer<'_, O>)
    where
        T: Sized,
    {
        <T as Write>::write_payload(items, w);
    }
}

// -- Wrappers ---------------------------------------------------------------

impl<'de, A, T> ReadAs<'de, Option<T>> for Option<A>
where
    A: ReadAs<'de, T>,
    T: Default,
{
    #[inline]
    fn read<O: Options>(value: &mut Option<T>, r: &mut Reader<'de, O>) -> PResult<()> {
        if r.try_null()? {
            *value = None;
            return Ok(());
        }
        match value {
            // Read over the existing payload so its allocations survive.
            Some(v) => A::read(v, r),
            None => {
                let mut v = T::default();
                A::read(&mut v, r)?;
                *value = Some(v);
                Ok(())
            }
        }
    }
}

impl<A, T> WriteAs<Option<T>> for Option<A>
where
    A: WriteAs<T>,
{
    #[inline]
    fn write<O: Options>(value: &Option<T>, w: &mut Writer<'_, O>) {
        match value {
            Some(v) => A::write(v, w),
            None => w.write_null(),
        }
    }

    #[inline]
    fn is_null(value: &Option<T>) -> bool {
        match value {
            Some(v) => A::is_null(v),
            None => true,
        }
    }
}

impl<'de, A, T> ReadAs<'de, Box<T>> for Box<A>
where
    A: ReadAs<'de, T>,
{
    #[inline]
    fn read<O: Options>(value: &mut Box<T>, r: &mut Reader<'de, O>) -> PResult<()> {
        A::read(&mut **value, r)
    }
}

/// Reference-counted payloads are read in place when this handle is the only
/// one, and replaced when it is shared, mirroring `impl_read_shared`.
macro_rules! impl_read_shared_as {
    ($($ty:ident),* $(,)?) => {$(
        impl<'de, A, T> ReadAs<'de, $ty<T>> for $ty<A>
        where
            A: ReadAs<'de, T>,
            T: Default,
        {
            #[inline]
            fn read<O: Options>(value: &mut $ty<T>, r: &mut Reader<'de, O>) -> PResult<()> {
                if let Some(v) = $ty::get_mut(value) {
                    return A::read(v, r);
                }
                let mut v = T::default();
                A::read(&mut v, r)?;
                *value = $ty::new(v);
                Ok(())
            }
        }
    )*}
}
impl_read_shared_as!(Rc, Arc);

/// A wrapper writes exactly what it points at, through the adapter that
/// describes the pointee.
macro_rules! impl_write_deref_as {
    ($($self:ty, $target:ty);* $(;)?) => {$(
        impl<A, T: ?Sized> WriteAs<$target> for $self
        where
            A: WriteAs<T>,
        {
            #[inline]
            fn write<O: Options>(value: &$target, w: &mut Writer<'_, O>) {
                A::write(&**value, w);
            }

            #[inline]
            fn is_null(value: &$target) -> bool {
                A::is_null(&**value)
            }
        }
    )*}
}
impl_write_deref_as!(Box<A>, Box<T>; Rc<A>, Rc<T>; Arc<A>, Arc<T>);

// -- Sequences --------------------------------------------------------------

/// Read over elements that are already here before growing, exactly as the
/// plain `Vec` and `VecDeque` impls do.
///
/// `Vec` also offers the block to the adapter first, which is the one thing
/// that differs between the two and the reason the bulk hook is named here.
/// `VecDeque` has no such offer to make: its storage is a ring, so there is no
/// one run of memory a payload could be copied into.
macro_rules! impl_read_seq_as {
    ($($ty:ident: $push:ident $(=> $bulk:ident)?),* $(,)?) => {$(
        impl<'de, A, T> ReadAs<'de, $ty<T>> for $ty<A>
        where
            A: ReadAs<'de, T>,
            T: Default,
        {
            fn read<O: Options>(value: &mut $ty<T>, r: &mut Reader<'de, O>) -> PResult<()> {
                $(
                    // The whole array in one copy, when the adapter says the
                    // stored elements are already the values. Declines without
                    // consuming anything otherwise, which is what every
                    // adapter that leaves the hook alone does.
                    if r.$bulk::<A, _>(value)? {
                        return Ok(());
                    }
                )?
                let held = value.len();
                let out = &mut *value;
                let n = r.read_seq_counted(|n| {
                    out.reserve(cautious::<T>(n).saturating_sub(held));
                    move |r, i| {
                        if i < held {
                            A::read(&mut out[i], r)
                        } else {
                            let mut v = T::default();
                            A::read(&mut v, r)?;
                            out.$push(v);
                            Ok(())
                        }
                    }
                })?;
                value.truncate(n);
                Ok(())
            }
        }
    )*}
}
impl_read_seq_as!(Vec: push => try_bulk_with, VecDeque: push_back);

/// A fixed-length array has every element already, so the adapter never needs
/// `T: Default` here.
impl<'de, A, T, const N: usize> ReadAs<'de, [T; N]> for [A; N]
where
    A: ReadAs<'de, T>,
{
    fn read<O: Options>(value: &mut [T; N], r: &mut Reader<'de, O>) -> PResult<()> {
        let n = r.read_seq(|r, i| {
            if i >= N {
                return Err(ErrorCode::ArrayLengthMismatch);
            }
            A::read(&mut value[i], r)
        })?;
        if n != N {
            return Err(ErrorCode::ArrayLengthMismatch);
        }
        Ok(())
    }
}

/// A set has no positional storage to reuse, so it is emptied and refilled.
macro_rules! impl_read_set_as {
    ($([$($gen:tt)*] $self:ty, $target:ty $(, reserve $reserve:ident)?);* $(;)?) => {$(
        impl<'de, A, T: Default $($gen)*> ReadAs<'de, $target> for $self
        where
            A: ReadAs<'de, T>,
        {
            fn read<O: Options>(value: &mut $target, r: &mut Reader<'de, O>) -> PResult<()> {
                value.clear();
                r.read_seq_counted(|_n| {
                    $( value.$reserve(cautious::<T>(_n)); )?
                    move |r, _| {
                        let mut v = T::default();
                        A::read(&mut v, r)?;
                        value.insert(v);
                        Ok(())
                    }
                })?;
                Ok(())
            }
        }
    )*}
}
impl_read_set_as!(
    [+ Eq + Hash, S: BuildHasher + Default] HashSet<A>, HashSet<T, S>, reserve reserve;
    [+ Ord] BTreeSet<A>, BTreeSet<T>;
);

/// Contiguous sequences become typed arrays when the *adapter* has one.
macro_rules! impl_write_slice_as {
    ($([$($gen:tt)*] $self:ty, $target:ty);* $(;)?) => {$(
        impl<A, T $($gen)*> WriteAs<$target> for $self
        where
            A: WriteAs<T>,
        {
            #[inline]
            fn write<O: Options>(value: &$target, w: &mut Writer<'_, O>) {
                w.write_slice_with::<A, _>(&value[..]);
            }
        }
    )*}
}
impl_write_slice_as!([] Vec<A>, Vec<T>; [, const N: usize] [A; N], [T; N]);

/// Sequences with no single backing slice are generic arrays, exactly as their
/// unadapted impls are.
macro_rules! impl_write_iter_as {
    ($([$($gen:tt)*] $self:ty, $target:ty);* $(;)?) => {$(
        impl<A, T $($gen)*> WriteAs<$target> for $self
        where
            A: WriteAs<T>,
        {
            #[inline]
            fn write<O: Options>(value: &$target, w: &mut Writer<'_, O>) {
                w.write_iter_with::<A, _, _>(value.len(), value.iter());
            }
        }
    )*}
}
impl_write_iter_as!(
    [] VecDeque<A>, VecDeque<T>;
    [, S] HashSet<A>, HashSet<T, S>;
    [] BTreeSet<A>, BTreeSet<T>;
);

// -- Maps -------------------------------------------------------------------

impl<T: FromBeveKey> ReadKeyAs<T> for Same {
    #[inline]
    fn from_key(key: Key<'_>) -> PResult<T> {
        T::from_key(key)
    }
}

impl<T: ToBeveKey + ?Sized> WriteKeyAs<T> for Same {
    const OBJECT: u8 = T::OBJECT;

    #[inline]
    fn write_key<O: Options>(value: &T, w: &mut Writer<'_, O>) {
        value.write_key(w);
    }
}

/// A map takes an adapter per half. The object header comes from the key
/// adapter, since an adapter that turns a string key into an integer one has
/// changed which kind of object this is.
macro_rules! impl_map_as {
    ($([$($kb:tt)*] [$($rgen:tt)*] [$($wgen:tt)*] $self:ty, $target:ty $(, reserve $reserve:ident)?);* $(;)?) => {$(
        impl<'de, KA, VA, K $($kb)*, V: Default $($rgen)*> ReadAs<'de, $target> for $self
        where
            KA: ReadKeyAs<K>,
            VA: ReadAs<'de, V>,
        {
            fn read<O: Options>(value: &mut $target, r: &mut Reader<'de, O>) -> PResult<()> {
                value.clear();
                r.read_map_counted(|_n| {
                    $( value.$reserve(cautious::<(K, V)>(_n)); )?
                    move |r, key| {
                        let k = KA::from_key(key)?;
                        let mut v = V::default();
                        VA::read(&mut v, r)?;
                        value.insert(k, v);
                        Ok(())
                    }
                })
            }
        }

        impl<KA, VA, K, V $($wgen)*> WriteAs<$target> for $self
        where
            KA: WriteKeyAs<K>,
            VA: WriteAs<V>,
        {
            #[inline]
            fn write<O: Options>(value: &$target, w: &mut Writer<'_, O>) {
                w.write_keyed_with::<KA, VA, _, _, _>(value.len(), value.iter());
            }
        }
    )*}
}
impl_map_as!(
    [: Eq + Hash] [, S: BuildHasher + Default] [, S] HashMap<KA, VA>, HashMap<K, V, S>, reserve reserve;
    [: Ord] [] [] BTreeMap<KA, VA>, BTreeMap<K, V>;
);
