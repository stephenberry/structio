//! [`Read`] and [`Write`] for the standard library's types.
//!
//! Reading is always *into* an existing value. Containers reuse the storage
//! they already hold: a `Vec` reads over its existing elements before pushing,
//! and a `String` field refills its own buffer. Parsing the same document shape
//! repeatedly into the same value settles into zero allocations.
//!
//! Types that must be constructed during a read (`Option`'s payload, a `Vec`'s
//! new tail) require `Default`, which is the same requirement Glaze places on
//! the types it deserializes.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::hash::{BuildHasher, Hash};
use std::rc::Rc;
use std::sync::Arc;

use crate::error::{ErrorCode, PResult};
use crate::json::parser::{JsonStr, Parser};
use crate::json::traits::{
    Read, ReadArray, ReadAs, ReadKeyAs, Write, WriteArray, WriteAs, WriteKeyAs,
};
use crate::json::writer::Writer;
use crate::options::Options;
use crate::traits::Same;

// ---------------------------------------------------------------------------
// Object keys
// ---------------------------------------------------------------------------

/// A type usable as a JSON object key.
///
/// JSON keys are always strings, so a numeric key is written quoted and parsed
/// back out of the quoted text.
pub trait FromJsonKey: Sized {
    fn from_key(key: &str) -> PResult<Self>;
}

/// The writing half of [`FromJsonKey`].
pub trait ToJsonKey {
    fn write_key<O: Options>(&self, w: &mut Writer<'_, O>);
}

impl FromJsonKey for String {
    #[inline]
    fn from_key(key: &str) -> PResult<Self> {
        Ok(key.to_owned())
    }
}

/// Every string-like key is already in JSON's key form, so it only needs
/// quoting and escaping.
macro_rules! impl_str_key {
    ($([$($gen:tt)*] $ty:ty),* $(,)?) => {$(
        impl<$($gen)*> ToJsonKey for $ty {
            #[inline]
            fn write_key<O: Options>(&self, w: &mut Writer<'_, O>) {
                w.write_str(self);
            }
        }
    )*}
}
impl_str_key!([] String, [] str, [] &str, ['a] Cow<'a, str>);

impl FromJsonKey for char {
    #[inline]
    fn from_key(key: &str) -> PResult<Self> {
        let mut it = key.chars();
        match (it.next(), it.next()) {
            (Some(c), None) => Ok(c),
            _ => Err(ErrorCode::ExpectedSingleChar),
        }
    }
}

impl ToJsonKey for char {
    #[inline]
    fn write_key<O: Options>(&self, w: &mut Writer<'_, O>) {
        let mut buf = [0u8; 4];
        w.write_str(self.encode_utf8(&mut buf));
    }
}

/// A numeric key is written through the widest signed or unsigned form, since
/// the quotes mean the digits are all that matter.
macro_rules! impl_int_key {
    ($wide:ty, $write:ident; $($t:ty),*) => {$(
        impl FromJsonKey for $t {
            #[inline]
            fn from_key(key: &str) -> PResult<Self> {
                key.parse::<$t>().map_err(|_| ErrorCode::InvalidNumber)
            }
        }
        impl ToJsonKey for $t {
            #[inline]
            fn write_key<O: Options>(&self, w: &mut Writer<'_, O>) {
                w.push(b'"');
                w.$write(*self as $wide);
                w.push(b'"');
            }
        }
    )*}
}
impl_int_key!(i128, write_i128_raw; i8, i16, i32, i64, isize, i128);
impl_int_key!(u128, write_u128; u8, u16, u32, u64, usize, u128);

// ---------------------------------------------------------------------------
// Scalars
// ---------------------------------------------------------------------------

impl<'de> Read<'de> for bool {
    #[inline]
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
        *self = p.read_bool()?;
        Ok(())
    }
}

impl Write for bool {
    #[inline]
    fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
        w.write_bool(*self);
    }
}

/// The unit type maps to `null`, so it can stand in for a field that carries
/// no information.
impl<'de> Read<'de> for () {
    #[inline]
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
        if p.try_null()? {
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

/// Every fixed-width integer narrower than 128 bits goes through the 64-bit
/// parser and writer, so the range check is a single `try_from` at the end
/// rather than a width-specific scan.
macro_rules! impl_int {
    ($wide:ty, $read:ident, $write:ident; $($t:ty),*) => {$(
        impl<'de> Read<'de> for $t {
            // Always, rather than a hint. This is what an array of integers
            // calls once per element, and a call here is dear: it spills the
            // parser's cursor to the stack and reloads it on return, which
            // costs more than the digits do. The body is two lines over a
            // reader that is itself shaped to be inlined; what pushes the
            // signed case past the threshold without this is the sign test and
            // the range narrowing, neither of which is worth a call.
            #[inline(always)]
            fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
                let v = p.$read()?;
                *self = <$t>::try_from(v).map_err(|_| ErrorCode::NumberOutOfRange)?;
                Ok(())
            }
        }
        impl Write for $t {
            #[inline]
            fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
                w.$write(*self as $wide);
            }
        }
    )*}
}
impl_int!(u64, read_u64, write_u64; u8, u16, u32, u64, usize);
impl_int!(i64, read_i64, write_i64; i8, i16, i32, i64, isize);

impl<'de> Read<'de> for u128 {
    #[inline]
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
        *self = p.read_u128()?;
        Ok(())
    }
}

impl Write for u128 {
    #[inline]
    fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
        w.write_u128(*self);
    }
}

impl<'de> Read<'de> for i128 {
    #[inline]
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
        *self = p.read_i128()?;
        Ok(())
    }
}

impl Write for i128 {
    #[inline]
    fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
        w.write_i128_raw(*self);
    }
}

impl<'de> Read<'de> for f64 {
    #[inline]
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
        *self = p.read_f64()?;
        Ok(())
    }
}

impl Write for f64 {
    #[inline]
    fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
        w.write_f64(*self);
    }
}

impl<'de> Read<'de> for f32 {
    #[inline]
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
        *self = p.read_f32()?;
        Ok(())
    }
}

impl Write for f32 {
    #[inline]
    fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
        w.write_f32(*self);
    }
}

// ---------------------------------------------------------------------------
// Strings
// ---------------------------------------------------------------------------

impl<'de> Read<'de> for String {
    #[inline]
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
        p.read_string_into(self)
    }
}

macro_rules! impl_write_str {
    ($([$($gen:tt)*] $ty:ty),* $(,)?) => {$(
        impl<$($gen)*> Write for $ty {
            #[inline]
            fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
                w.write_str(self);
            }
        }
    )*}
}
impl_write_str!([] String, [] str, ['a] Cow<'a, str>);

/// Borrow a string straight out of the input, with no copy at all.
///
/// A JSON string containing escapes has no representation as a subslice of the
/// document, so this reports [`ErrorCode::EscapeInBorrowedString`] rather than
/// allocating behind the caller's back. Use `Cow<str>` to accept both.
impl<'de> Read<'de> for &'de str {
    #[inline]
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
        *self = p.read_str()?;
        Ok(())
    }
}

impl<'de> Read<'de> for Cow<'de, str> {
    #[inline]
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
        *self = match p.read_string()? {
            JsonStr::Borrowed(s) => Cow::Borrowed(s),
            JsonStr::Owned(s) => Cow::Owned(s),
        };
        Ok(())
    }
}

impl<'de> Read<'de> for char {
    #[inline]
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
        let s = p.read_string()?;
        let mut it = s.as_str().chars();
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
    #[inline]
    fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
        let mut buf = [0u8; 4];
        w.write_str(self.encode_utf8(&mut buf));
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
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
        if p.try_null()? {
            *self = None;
            return Ok(());
        }
        match self {
            // Read over the existing payload so its allocations survive.
            Some(v) => v.read(p),
            None => {
                let mut v = T::default();
                v.read(p)?;
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
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
        (**self).read(p)
    }
}

/// Reference-counted payloads are read in place when this handle is the only
/// one, matching `Box` and keeping the promise that a read reuses what you
/// already own. A shared payload cannot be touched, so it is replaced.
macro_rules! impl_read_shared {
    ($($ty:ident),* $(,)?) => {$(
        impl<'de, T: Read<'de> + Default> Read<'de> for $ty<T> {
            #[inline]
            fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
                if let Some(v) = $ty::get_mut(self) {
                    return v.read(p);
                }
                let mut v = T::default();
                v.read(p)?;
                *self = $ty::new(v);
                Ok(())
            }
        }
    )*}
}
impl_read_shared!(Rc, Arc);

/// A wrapper writes exactly what it points at.
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
///
/// An element past what was held is pushed as a default first and then read
/// in place, the same way as one that was already here, so the element read
/// has one call site rather than two. That halves what the compiler is asked
/// to inline into this loop, and for a scalar element it is the difference
/// between the conversion sitting in the loop and a call per element.
macro_rules! impl_read_seq {
    ($($ty:ident: $push:ident),* $(,)?) => {$(
        impl<'de, T: Read<'de> + Default> Read<'de> for $ty<T> {
            #[inline]
            fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
                let held = self.len();
                let n = p.read_seq(|p, i| {
                    if i >= held {
                        self.$push(T::default());
                    }
                    self[i].read(p)
                })?;
                self.truncate(n);
                Ok(())
            }
        }
    )*}
}
impl_read_seq!(Vec: push, VecDeque: push_back);

/// A JSON array is text, so its elements are built rather than pointed at and
/// this is always the owned half. It exists so that a type holding a
/// [`Cow<[T]>`](Cow) for the sake of [BEVE](crate::beve::Reader::try_slice),
/// where the borrowed half is the point, still reads from JSON.
impl<'de, T: Read<'de> + Default + Clone> Read<'de> for Cow<'de, [T]> {
    #[inline]
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
        match self {
            // Read over what is owned here, so the allocation survives.
            Cow::Owned(v) => v.read(p),
            Cow::Borrowed(_) => {
                let mut v = Vec::new();
                v.read(p)?;
                *self = Cow::Owned(v);
                Ok(())
            }
        }
    }
}

impl<'de, T: Read<'de>, const N: usize> Read<'de> for [T; N] {
    #[inline]
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
        let n = p.read_seq(|p, i| {
            if i >= N {
                return Err(ErrorCode::ArrayLengthMismatch);
            }
            self[i].read(p)
        })?;
        if n != N {
            return Err(ErrorCode::ArrayLengthMismatch);
        }
        Ok(())
    }
}

/// A set has no positional storage to reuse, so it is emptied and refilled.
macro_rules! impl_read_set {
    ($([$($gen:tt)*] $ty:ty),* $(,)?) => {$(
        impl<'de, T: Read<'de> + Default $($gen)*> Read<'de> for $ty {
            #[inline]
            fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
                self.clear();
                p.read_seq(|p, _| {
                    let mut v = T::default();
                    v.read(p)?;
                    self.insert(v);
                    Ok(())
                })?;
                Ok(())
            }
        }
    )*}
}
impl_read_set!(
    [+ Eq + Hash, S: BuildHasher + Default] HashSet<T, S>,
    [+ Ord] BTreeSet<T>,
);

/// Anything iterable in order writes as a JSON array.
macro_rules! impl_write_as_array {
    ($([$($gen:tt)*] $ty:ty),* $(,)?) => {$(
        impl<T: Write $($gen)*> Write for $ty {
            #[inline]
            fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
                w.write_seq(self.iter());
            }
        }
    )*}
}
impl_write_as_array!(
    [] Vec<T>,
    [+ Clone] Cow<'_, [T]>,
    [] [T],
    [] VecDeque<T>,
    [, const N: usize] [T; N],
    [, S] HashSet<T, S>,
    [] BTreeSet<T>,
);

// ---------------------------------------------------------------------------
// Maps
// ---------------------------------------------------------------------------

/// JSON object keys are strings, so the key type converts through
/// [`FromJsonKey`]/[`ToJsonKey`] rather than being parsed as a value.
macro_rules! impl_map {
    ($([$($kb:tt)*] [$($rgen:tt)*] [$($wgen:tt)*] $ty:ty),* $(,)?) => {$(
        impl<'de, K: FromJsonKey $($kb)*, V: Read<'de> + Default $($rgen)*> Read<'de> for $ty {
            #[inline]
            fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
                self.clear();
                p.read_map(|p, key| {
                    let k = K::from_key(key.as_str())?;
                    let mut v = V::default();
                    v.read(p)?;
                    self.insert(k, v);
                    Ok(())
                })
            }
        }

        impl<K: ToJsonKey, V: Write $($wgen)*> Write for $ty {
            #[inline]
            fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
                w.write_keyed(self.iter());
            }
        }
    )*}
}
impl_map!(
    [+ Eq + Hash] [, S: BuildHasher + Default] [, S] HashMap<K, V, S>,
    [+ Ord] [] [] BTreeMap<K, V>,
);

// ---------------------------------------------------------------------------
// Tuples, as JSON arrays
// ---------------------------------------------------------------------------

macro_rules! impl_tuple {
    ($n:expr; $($name:ident $idx:tt),+) => {
        impl<'de, $($name: Read<'de>),+> ReadArray<'de> for ($($name,)+) {
            #[inline]
            fn read_element<O: Options>(&mut self, index: usize, p: &mut Parser<'de, O>) -> PResult<()> {
                // A fixed-width tuple maps to a fixed-length array, so the
                // element index selects the member directly.
                $(if index == $idx { return self.$idx.read(p); })+
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
            #[inline]
            fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
                p.read_array(self)
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
// The mirror of everything above, one trait pair further out: `ReadAs`/
// `WriteAs` are implemented by an adapter *about* a type rather than by the
// type, which is what lets a field keep a type this crate has never heard of.
// See `docs/schemas.md`.
//
// Each impl below mirrors the plain impl of the same container, including
// which allocations a read reuses and which bounds it needs, and they appear in
// the same order. Coherence rests on every one of them having a different
// `Self`: `Same`, `Option<A>`, `Vec<A>` and a user's own `Base64` are pairwise
// disjoint, so a whole-container adapter over `Vec<u8>` can sit in the same
// declaration as an element-wise `Vec<A>`.
//
// `T: Default` runs through the read half wherever a container builds an
// element it did not already hold, exactly as the plain impls require it. A
// newtype discharges that with one impl and an adapter has nowhere to put one,
// so it is the sharp edge of the mechanism rather than a detail.

impl<'de, T: Read<'de>> ReadAs<'de, T> for Same {
    #[inline]
    fn read<O: Options>(value: &mut T, p: &mut Parser<'de, O>) -> PResult<()> {
        value.read(p)
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
}

// -- Wrappers ---------------------------------------------------------------

impl<'de, A, T> ReadAs<'de, Option<T>> for Option<A>
where
    A: ReadAs<'de, T>,
    T: Default,
{
    #[inline]
    fn read<O: Options>(value: &mut Option<T>, p: &mut Parser<'de, O>) -> PResult<()> {
        if p.try_null()? {
            *value = None;
            return Ok(());
        }
        match value {
            // Read over the existing payload so its allocations survive.
            Some(v) => A::read(v, p),
            None => {
                let mut v = T::default();
                A::read(&mut v, p)?;
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

    /// `None` is absent whatever the adapter says, and a `Some` is whatever the
    /// adapter says its payload is, exactly as `Option<T>` defers to `T`.
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
    fn read<O: Options>(value: &mut Box<T>, p: &mut Parser<'de, O>) -> PResult<()> {
        A::read(&mut **value, p)
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
            fn read<O: Options>(value: &mut $ty<T>, p: &mut Parser<'de, O>) -> PResult<()> {
                if let Some(v) = $ty::get_mut(value) {
                    return A::read(v, p);
                }
                let mut v = T::default();
                A::read(&mut v, p)?;
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

/// Read over elements that are already here before growing, exactly as
/// `impl_read_seq` does, so an adapted sequence of `String` reuses every buffer
/// it is holding.
macro_rules! impl_read_seq_as {
    ($($ty:ident: $push:ident),* $(,)?) => {$(
        impl<'de, A, T> ReadAs<'de, $ty<T>> for $ty<A>
        where
            A: ReadAs<'de, T>,
            T: Default,
        {
            #[inline]
            fn read<O: Options>(value: &mut $ty<T>, p: &mut Parser<'de, O>) -> PResult<()> {
                let held = value.len();
                let n = p.read_seq(|p, i| {
                    if i >= held {
                        value.$push(T::default());
                    }
                    A::read(&mut value[i], p)
                })?;
                value.truncate(n);
                Ok(())
            }
        }
    )*}
}
impl_read_seq_as!(Vec: push, VecDeque: push_back);

/// A fixed-length array has every element already, so the adapter never needs
/// `T: Default` here. Length is checked rather than filled, as it is for
/// `[T; N]` itself.
impl<'de, A, T, const N: usize> ReadAs<'de, [T; N]> for [A; N]
where
    A: ReadAs<'de, T>,
{
    #[inline]
    fn read<O: Options>(value: &mut [T; N], p: &mut Parser<'de, O>) -> PResult<()> {
        let n = p.read_seq(|p, i| {
            if i >= N {
                return Err(ErrorCode::ArrayLengthMismatch);
            }
            A::read(&mut value[i], p)
        })?;
        if n != N {
            return Err(ErrorCode::ArrayLengthMismatch);
        }
        Ok(())
    }
}

/// A set has no positional storage to reuse, so it is emptied and refilled.
macro_rules! impl_read_set_as {
    ($([$($gen:tt)*] $self:ty, $target:ty);* $(;)?) => {$(
        impl<'de, A, T: Default $($gen)*> ReadAs<'de, $target> for $self
        where
            A: ReadAs<'de, T>,
        {
            #[inline]
            fn read<O: Options>(value: &mut $target, p: &mut Parser<'de, O>) -> PResult<()> {
                value.clear();
                p.read_seq(|p, _| {
                    let mut v = T::default();
                    A::read(&mut v, p)?;
                    value.insert(v);
                    Ok(())
                })?;
                Ok(())
            }
        }
    )*}
}
impl_read_set_as!(
    [+ Eq + Hash, S: BuildHasher + Default] HashSet<A>, HashSet<T, S>;
    [+ Ord] BTreeSet<A>, BTreeSet<T>;
);

/// Anything iterable in order writes as a JSON array, its elements through the
/// adapter. Nothing beyond `A: WriteAs<T>` is asked of `T`, which is the
/// asymmetry with the read half.
macro_rules! impl_write_as_array_as {
    ($([$($gen:tt)*] $self:ty, $target:ty);* $(;)?) => {$(
        impl<A, T $($gen)*> WriteAs<$target> for $self
        where
            A: WriteAs<T>,
        {
            #[inline]
            fn write<O: Options>(value: &$target, w: &mut Writer<'_, O>) {
                w.write_seq_with::<A, _, _>(value.iter());
            }
        }
    )*}
}
impl_write_as_array_as!(
    [] Vec<A>, Vec<T>;
    [] VecDeque<A>, VecDeque<T>;
    [, const N: usize] [A; N], [T; N];
    [, S] HashSet<A>, HashSet<T, S>;
    [] BTreeSet<A>, BTreeSet<T>;
);

// -- Maps -------------------------------------------------------------------

impl<T: FromJsonKey> ReadKeyAs<T> for Same {
    #[inline]
    fn from_key(key: &str) -> PResult<T> {
        T::from_key(key)
    }
}

impl<T: ToJsonKey + ?Sized> WriteKeyAs<T> for Same {
    #[inline]
    fn write_key<O: Options>(value: &T, w: &mut Writer<'_, O>) {
        value.write_key(w);
    }
}

/// A map takes an adapter per half, so `HashMap<Same, Millis>` adapts the
/// values and leaves the keys to [`FromJsonKey`]/[`ToJsonKey`].
macro_rules! impl_map_as {
    ($([$($kb:tt)*] [$($rgen:tt)*] [$($wgen:tt)*] $self:ty, $target:ty);* $(;)?) => {$(
        impl<'de, KA, VA, K $($kb)*, V: Default $($rgen)*> ReadAs<'de, $target> for $self
        where
            KA: ReadKeyAs<K>,
            VA: ReadAs<'de, V>,
        {
            #[inline]
            fn read<O: Options>(value: &mut $target, p: &mut Parser<'de, O>) -> PResult<()> {
                value.clear();
                p.read_map(|p, key| {
                    let k = KA::from_key(key.as_str())?;
                    let mut v = V::default();
                    VA::read(&mut v, p)?;
                    value.insert(k, v);
                    Ok(())
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
                w.write_keyed_with::<KA, VA, _, _, _>(value.iter());
            }
        }
    )*}
}
impl_map_as!(
    [: Eq + Hash] [, S: BuildHasher + Default] [, S] HashMap<KA, VA>, HashMap<K, V, S>;
    [: Ord] [] [] BTreeMap<KA, VA>, BTreeMap<K, V>;
);
