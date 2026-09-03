//! A value whose shape is not known at compile time.
//!
//! Everything else in this crate reads straight into a type you declared.
//! [`Value`] is for the value that has no such type: a register tree a
//! device publishes and a host walks by path, a body a coordinator forwards
//! without reading, a setting stored under a key some plugin chose. It is an
//! ordinary tree, null, bool, number, string, array or object, with the
//! accessors such a tree needs and nothing that would make it a substitute
//! for a declared type.
//!
//! It reads and writes through both formats like any other type, so it can be
//! a field of an [`object!`](crate::object) declaration, a whole document, or
//! an element of a `Vec`. A BEVE document read into it keeps its numbers as
//! numbers; a typed array becomes an array of them, a complex value becomes
//! `[re, im]` pairs, and a matrix becomes the `{layout, extents, value}` object
//! both formats already read one back from.
//!
//! [`to_value`] and [`from_value`] move a declared type in and out of the
//! tree. They go through JSON text, which is the one representation both
//! sides already speak, and cost accordingly: a `Value` is the right shape for
//! something nothing here decodes, not a faster route to something that is.
//!
//! # Numbers
//!
//! A [`Number`] remembers which of three things it holds, an unsigned integer,
//! a negative integer, or a float, so that an integer read from a document
//! comes back as one. `1` and `1.0` are different numbers, as they are
//! different tokens, and a `Value` writes a whole-valued float as `1.0` so
//! that the distinction survives a trip through text. That is the one place
//! its text differs from a declared type's, whose `f64` writes `1`; it follows
//! that a whole-valued `f64` arriving through [`to_value`] is classified as an
//! integer, the text having lost what it was. `-0` is the integer zero. A
//! non-finite float has no JSON form and cannot be stored: `From<f64>` yields
//! [`Value::Null`] for one, and reading one, from a BEVE float or a JSON
//! literal past `f64`'s range, is [`NumberOutOfRange`](ErrorCode::NumberOutOfRange).
//!
//! # Keys
//!
//! An object's keys are kept sorted, so a value written and read back
//! reproduces its bytes whatever order it was built in. BEVE objects keyed by
//! integers read as strings of their digits, the one form JSON has for a key.

use core::fmt;
use core::ops;
use core::str::FromStr;
use std::collections::BTreeMap;

use crate::beve::header::{self, byte_width};
use crate::beve::reader::{
    Typed, bf16_to_f32, complex_payload, f16_to_f32, key_width, le_u128, payload_len, sign_extend,
};
use crate::beve::{self, Reader as BeveReader, Writer as BeveWriter, cautious};
use crate::error::{ErrorCode, PResult, Result};
use crate::ext::MatrixLayout;
use crate::json::{self, Parser, Writer as JsonWriter};
use crate::num::dtoa::{MAX_FLOAT_BYTES, write_f64};
use crate::options::{Options, Pretty, Standard};

/// The members of a [`Value::Object`], sorted by key.
pub type Object = BTreeMap<String, Value>;

/// A value of unknown shape. See the [module docs](self).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Value>),
    Object(Object),
}

/// A JSON number that knows whether it is an integer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Number(Repr);

impl Default for Number {
    /// Zero, as an unsigned integer.
    fn default() -> Self {
        Number(Repr::Unsigned(0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Repr {
    Unsigned(u64),
    Signed(i64),
    Float(f64),
}

impl Number {
    /// A finite float, or `None` for NaN and the infinities, which have no
    /// JSON form.
    pub fn from_f64(v: f64) -> Option<Self> {
        v.is_finite().then_some(Number(Repr::Float(v)))
    }

    /// The value as a `u64`, if it is a non-negative integer.
    pub fn as_u64(&self) -> Option<u64> {
        match self.0 {
            Repr::Unsigned(v) => Some(v),
            _ => None,
        }
    }

    /// The value as an `i64`, if it is an integer in range.
    pub fn as_i64(&self) -> Option<i64> {
        match self.0 {
            Repr::Unsigned(v) => i64::try_from(v).ok(),
            Repr::Signed(v) => Some(v),
            Repr::Float(_) => None,
        }
    }

    /// The value as an `f64`. An integer widens; one past 2^53 rounds.
    pub fn as_f64(&self) -> Option<f64> {
        Some(match self.0 {
            Repr::Unsigned(v) => v as f64,
            Repr::Signed(v) => v as f64,
            Repr::Float(v) => v,
        })
    }

    /// Whether the number is a non-negative integer.
    pub fn is_u64(&self) -> bool {
        matches!(self.0, Repr::Unsigned(_))
    }

    /// Whether the number is an integer that fits an `i64`.
    pub fn is_i64(&self) -> bool {
        self.as_i64().is_some()
    }

    /// Whether the number was a float.
    pub fn is_f64(&self) -> bool {
        matches!(self.0, Repr::Float(_))
    }

    /// Parse the text of a JSON number token.
    ///
    /// An integer that does not fit its 64-bit type is kept as a float rather
    /// than refused, the way a `JSON.parse` would keep it.
    fn from_token(s: &str) -> PResult<Self> {
        let is_float = s.bytes().any(|b| matches!(b, b'.' | b'e' | b'E'));
        if !is_float {
            if let Some(rest) = s.strip_prefix('-') {
                if let Ok(v) = rest.parse::<i64>().map(|v| -v) {
                    return Ok(Number::from(v));
                }
                if let Ok(v) = s.parse::<i64>() {
                    return Ok(Number::from(v));
                }
            } else if let Ok(v) = s.parse::<u64>() {
                return Ok(Number(Repr::Unsigned(v)));
            }
        }
        // The scanner passed only a JSON number literal, which `f64` always
        // parses; what it cannot do is hold one past its range.
        let v: f64 = s.parse().map_err(|_| ErrorCode::InvalidNumber)?;
        Number::from_f64(v).ok_or(ErrorCode::NumberOutOfRange)
    }

    /// A float's JSON text, always carrying a `.` or an exponent so it reads
    /// back as a float. The crate's writer gives `1.0` the shortest form,
    /// `1`, which is right for a declared `f64` and wrong for a number whose
    /// point is that it remembers its kind.
    fn float_token(v: f64, buf: &mut [u8; MAX_FLOAT_BYTES]) -> &str {
        // Finite by construction; `write_f64` only refuses non-finite input.
        let mut n = write_f64(v, buf).expect("a stored float is finite");
        if !buf[..n].iter().any(|b| matches!(b, b'.' | b'e' | b'E')) {
            buf[n] = b'.';
            buf[n + 1] = b'0';
            n += 2;
        }
        // ASCII in, ASCII out.
        core::str::from_utf8(&buf[..n]).expect("a number is ASCII")
    }
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Repr::Unsigned(v) => write!(f, "{v}"),
            Repr::Signed(v) => write!(f, "{v}"),
            Repr::Float(v) => f.write_str(Number::float_token(v, &mut [0; MAX_FLOAT_BYTES])),
        }
    }
}

macro_rules! number_from_unsigned {
    ($($t:ty),*) => {$(
        impl From<$t> for Number {
            fn from(v: $t) -> Self { Number(Repr::Unsigned(v as u64)) }
        }
        impl From<$t> for Value {
            fn from(v: $t) -> Self { Value::Number(Number::from(v)) }
        }
    )*};
}

macro_rules! number_from_signed {
    ($($t:ty),*) => {$(
        impl From<$t> for Number {
            fn from(v: $t) -> Self {
                if v < 0 { Number(Repr::Signed(v as i64)) } else { Number(Repr::Unsigned(v as u64)) }
            }
        }
        impl From<$t> for Value {
            fn from(v: $t) -> Self { Value::Number(Number::from(v)) }
        }
    )*};
}

number_from_unsigned!(u8, u16, u32, u64, usize);
number_from_signed!(i8, i16, i32, i64, isize);

impl From<f64> for Value {
    /// A non-finite float becomes [`Value::Null`].
    fn from(v: f64) -> Self {
        Number::from_f64(v).map_or(Value::Null, Value::Number)
    }
}

impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Value::from(v as f64)
    }
}

/// So that `value!` can take a `&Value` where it takes any expression; it is
/// a clone.
impl From<&Value> for Value {
    fn from(v: &Value) -> Self {
        v.clone()
    }
}

impl From<Number> for Value {
    fn from(v: Number) -> Self {
        Value::Number(v)
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::String(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::String(v.to_owned())
    }
}

impl From<&String> for Value {
    fn from(v: &String) -> Self {
        Value::String(v.clone())
    }
}

impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(v: Vec<T>) -> Self {
        Value::Array(v.into_iter().map(Into::into).collect())
    }
}

impl<T: Clone + Into<Value>> From<&[T]> for Value {
    fn from(v: &[T]) -> Self {
        Value::Array(v.iter().cloned().map(Into::into).collect())
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Value::Null, Into::into)
    }
}

impl From<Object> for Value {
    fn from(v: Object) -> Self {
        Value::Object(v)
    }
}

impl From<()> for Value {
    fn from((): ()) -> Self {
        Value::Null
    }
}

impl<T: Into<Value>> FromIterator<T> for Value {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Value::Array(iter.into_iter().map(Into::into).collect())
    }
}

impl<K: Into<String>, V: Into<Value>> FromIterator<(K, V)> for Value {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Value::Object(
            iter.into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }
}

impl PartialEq<str> for Value {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == Some(other)
    }
}

impl PartialEq<&str> for Value {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == Some(*other)
    }
}

impl PartialEq<bool> for Value {
    fn eq(&self, other: &bool) -> bool {
        self.as_bool() == Some(*other)
    }
}

// ---------------------------------------------------------------------------
// Accessors
// ---------------------------------------------------------------------------

/// What can index a [`Value`]: a key into an object, or a position into an
/// array. Sealed; the two impls are the whole set.
pub trait Index: private::Sealed {
    #[doc(hidden)]
    fn index_into<'a>(&self, doc: &'a Value) -> Option<&'a Value>;
    #[doc(hidden)]
    fn index_into_mut<'a>(&self, doc: &'a mut Value) -> Option<&'a mut Value>;
    #[doc(hidden)]
    fn index_or_insert<'a>(&self, doc: &'a mut Value) -> &'a mut Value;
}

mod private {
    pub trait Sealed {}
    impl Sealed for str {}
    impl Sealed for String {}
    impl Sealed for usize {}
    impl<T: Sealed + ?Sized> Sealed for &T {}
}

impl Index for str {
    fn index_into<'a>(&self, doc: &'a Value) -> Option<&'a Value> {
        match doc {
            Value::Object(map) => map.get(self),
            _ => None,
        }
    }
    fn index_into_mut<'a>(&self, doc: &'a mut Value) -> Option<&'a mut Value> {
        match doc {
            Value::Object(map) => map.get_mut(self),
            _ => None,
        }
    }
    fn index_or_insert<'a>(&self, doc: &'a mut Value) -> &'a mut Value {
        if let Value::Null = doc {
            *doc = Value::Object(Object::new());
        }
        match doc {
            Value::Object(map) => map.entry(self.to_owned()).or_insert(Value::Null),
            other => panic!("cannot index a JSON {} with a key", other.kind()),
        }
    }
}

impl Index for String {
    fn index_into<'a>(&self, doc: &'a Value) -> Option<&'a Value> {
        self.as_str().index_into(doc)
    }
    fn index_into_mut<'a>(&self, doc: &'a mut Value) -> Option<&'a mut Value> {
        self.as_str().index_into_mut(doc)
    }
    fn index_or_insert<'a>(&self, doc: &'a mut Value) -> &'a mut Value {
        self.as_str().index_or_insert(doc)
    }
}

impl Index for usize {
    fn index_into<'a>(&self, doc: &'a Value) -> Option<&'a Value> {
        match doc {
            Value::Array(items) => items.get(*self),
            _ => None,
        }
    }
    fn index_into_mut<'a>(&self, doc: &'a mut Value) -> Option<&'a mut Value> {
        match doc {
            Value::Array(items) => items.get_mut(*self),
            _ => None,
        }
    }
    fn index_or_insert<'a>(&self, doc: &'a mut Value) -> &'a mut Value {
        match doc {
            Value::Array(items) => {
                let len = items.len();
                items.get_mut(*self).unwrap_or_else(|| {
                    panic!("cannot index a JSON array of length {len} at {self}")
                })
            }
            other => panic!("cannot index a JSON {} with a position", other.kind()),
        }
    }
}

impl<T: Index + ?Sized> Index for &T {
    fn index_into<'a>(&self, doc: &'a Value) -> Option<&'a Value> {
        (**self).index_into(doc)
    }
    fn index_into_mut<'a>(&self, doc: &'a mut Value) -> Option<&'a mut Value> {
        (**self).index_into_mut(doc)
    }
    fn index_or_insert<'a>(&self, doc: &'a mut Value) -> &'a mut Value {
        (**self).index_or_insert(doc)
    }
}

static NULL: Value = Value::Null;

impl<I: Index> ops::Index<I> for Value {
    type Output = Value;

    /// `doc["key"]` or `doc[3]`. A member that is not there, or a document
    /// that is not the container asked for, is [`Value::Null`] rather than
    /// a panic, so a chain of lookups reads like a path.
    fn index(&self, index: I) -> &Value {
        index.index_into(self).unwrap_or(&NULL)
    }
}

impl<I: Index> ops::IndexMut<I> for Value {
    /// `doc["key"] = value`. A missing key is inserted, and a `Null` document
    /// becomes an object first, so a tree can be built by assignment. Indexing
    /// any other kind by key, or an array past its end, panics.
    fn index_mut(&mut self, index: I) -> &mut Value {
        index.index_or_insert(self)
    }
}

impl Value {
    /// The member at `index`, if the document is the container for it.
    pub fn get<I: Index>(&self, index: I) -> Option<&Value> {
        index.index_into(self)
    }

    /// Mutable form of [`get`](Self::get).
    pub fn get_mut<I: Index>(&mut self, index: I) -> Option<&mut Value> {
        index.index_into_mut(self)
    }

    /// The value a [JSON Pointer] names, or `None` if the path does not
    /// resolve. `""` is the document itself.
    ///
    /// [JSON Pointer]: https://www.rfc-editor.org/rfc/rfc6901
    pub fn pointer(&self, pointer: &str) -> Option<&Value> {
        if pointer.is_empty() {
            return Some(self);
        }
        if !pointer.starts_with('/') {
            return None;
        }
        pointer
            .split('/')
            .skip(1)
            .map(unescape_token)
            .try_fold(self, |doc, token| match doc {
                Value::Object(map) => map.get(&token),
                Value::Array(items) => token.parse::<usize>().ok().and_then(|i| items.get(i)),
                _ => None,
            })
    }

    /// Mutable form of [`pointer`](Self::pointer).
    pub fn pointer_mut(&mut self, pointer: &str) -> Option<&mut Value> {
        if pointer.is_empty() {
            return Some(self);
        }
        if !pointer.starts_with('/') {
            return None;
        }
        pointer
            .split('/')
            .skip(1)
            .map(unescape_token)
            .try_fold(self, |doc, token| match doc {
                Value::Object(map) => map.get_mut(&token),
                Value::Array(items) => token.parse::<usize>().ok().and_then(|i| items.get_mut(i)),
                _ => None,
            })
    }

    /// The string, if it is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The bool, if it is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The number, if it is one.
    pub fn as_number(&self) -> Option<Number> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// The number as a `u64`, if it is a non-negative integer.
    pub fn as_u64(&self) -> Option<u64> {
        self.as_number().and_then(|n| n.as_u64())
    }

    /// The number as an `i64`, if it is an integer in range.
    pub fn as_i64(&self) -> Option<i64> {
        self.as_number().and_then(|n| n.as_i64())
    }

    /// The number as an `f64`, integers widened.
    pub fn as_f64(&self) -> Option<f64> {
        self.as_number().and_then(|n| n.as_f64())
    }

    /// `Some(())` for `null`, the shape the other accessors have.
    pub fn as_null(&self) -> Option<()> {
        matches!(self, Value::Null).then_some(())
    }

    /// The elements, if it is an array.
    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Mutable form of [`as_array`](Self::as_array).
    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    /// The members, if it is an object.
    pub fn as_object(&self) -> Option<&Object> {
        match self {
            Value::Object(map) => Some(map),
            _ => None,
        }
    }

    /// Mutable form of [`as_object`](Self::as_object).
    pub fn as_object_mut(&mut self) -> Option<&mut Object> {
        match self {
            Value::Object(map) => Some(map),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn is_bool(&self) -> bool {
        matches!(self, Value::Bool(_))
    }

    pub fn is_number(&self) -> bool {
        matches!(self, Value::Number(_))
    }

    pub fn is_u64(&self) -> bool {
        self.as_number().is_some_and(|n| n.is_u64())
    }

    pub fn is_i64(&self) -> bool {
        self.as_number().is_some_and(|n| n.is_i64())
    }

    pub fn is_f64(&self) -> bool {
        self.as_number().is_some_and(|n| n.is_f64())
    }

    pub fn is_string(&self) -> bool {
        matches!(self, Value::String(_))
    }

    pub fn is_array(&self) -> bool {
        matches!(self, Value::Array(_))
    }

    pub fn is_object(&self) -> bool {
        matches!(self, Value::Object(_))
    }

    /// Take the value out, leaving `Null` behind.
    pub fn take(&mut self) -> Value {
        core::mem::take(self)
    }

    /// Parse JSON text.
    pub fn from_json(text: &str) -> Result<Value> {
        json::from_str(text)
    }

    /// Compact JSON text. The same as `Display`.
    pub fn to_json(&self) -> String {
        json::to_string(self)
    }

    /// Indented JSON text.
    pub fn to_json_pretty(&self) -> String {
        json::to_string_with::<Pretty, _>(self)
    }

    /// Read a BEVE document.
    pub fn from_beve(bytes: &[u8]) -> Result<Value> {
        beve::from_slice(bytes)
    }

    /// Write as BEVE.
    pub fn to_beve(&self) -> Vec<u8> {
        beve::to_vec(self)
    }

    /// The kind, for a message.
    fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }
}

/// Undo a JSON Pointer token's two escapes, in the order the RFC requires.
fn unescape_token(token: &str) -> String {
    if token.contains('~') {
        token.replace("~1", "/").replace("~0", "~")
    } else {
        token.to_owned()
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            f.write_str(&self.to_json_pretty())
        } else {
            f.write_str(&self.to_json())
        }
    }
}

impl FromStr for Value {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self> {
        Value::from_json(s)
    }
}

// ---------------------------------------------------------------------------
// Declared types in and out
// ---------------------------------------------------------------------------

/// Build a document from a value that writes JSON.
///
/// Fails only for a value whose JSON does not read back as a document, which
/// a `Write` impl of this crate does not produce.
pub fn to_value<T: json::Write + ?Sized>(value: &T) -> Result<Value> {
    json::from_str(&json::to_string(value))
}

/// Read a value out of a document, under the [`Standard`] policy.
///
/// The document is written as JSON and read as `T` would be from text, so a
/// key the type does not declare is refused exactly as it would be from a
/// file. [`from_value_with`] takes another policy.
pub fn from_value<T>(doc: &Value) -> Result<T>
where
    T: for<'de> json::Read<'de> + Default,
{
    from_value_with::<Standard, T>(doc)
}

/// [`from_value`] under an explicit policy.
pub fn from_value_with<O, T>(doc: &Value) -> Result<T>
where
    O: Options,
    T: for<'de> json::Read<'de> + Default,
{
    json::from_str_with::<O, T>(&json::to_string(doc))
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

impl<'de> json::Read<'de> for Value {
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
        p.skip_ws();
        let Some(&b) = p.rest().first() else {
            return Err(ErrorCode::UnexpectedEnd);
        };
        *self = match b {
            b'{' => {
                let mut map = match self.take() {
                    Value::Object(mut map) => {
                        map.clear();
                        map
                    }
                    _ => Object::new(),
                };
                p.read_map(|p, key| {
                    let mut value = Value::Null;
                    value.read(p)?;
                    map.insert(key.into_string(), value);
                    Ok(())
                })?;
                Value::Object(map)
            }
            b'[' => {
                let mut items = match self.take() {
                    Value::Array(mut items) => {
                        items.clear();
                        items
                    }
                    _ => Vec::new(),
                };
                p.read_seq(|p, _| {
                    let mut value = Value::Null;
                    value.read(p)?;
                    items.push(value);
                    Ok(())
                })?;
                Value::Array(items)
            }
            b'"' => Value::String(p.read_string()?.into_string()),
            b't' | b'f' => Value::Bool(p.read_bool()?),
            b'n' => {
                if !p.try_null()? {
                    return Err(ErrorCode::ExpectedNull);
                }
                Value::Null
            }
            b'-' | b'0'..=b'9' => Value::Number(Number::from_token(p.read_number_str()?)?),
            _ => return Err(ErrorCode::UnexpectedCharacter),
        };
        Ok(())
    }
}

impl json::Write for Value {
    fn write<O: Options>(&self, w: &mut JsonWriter<'_, O>) {
        match self {
            Value::Null => w.write_null(),
            Value::Bool(b) => w.write_bool(*b),
            Value::Number(n) => n.write(w),
            Value::String(s) => w.write_str(s),
            Value::Array(items) => w.write_seq(items.iter()),
            Value::Object(map) => w.write_keyed(map.iter()),
        }
    }

    fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

impl<'de> json::Read<'de> for Number {
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> PResult<()> {
        p.skip_ws();
        *self = Number::from_token(p.read_number_str()?)?;
        Ok(())
    }
}

impl json::Write for Number {
    fn write<O: Options>(&self, w: &mut JsonWriter<'_, O>) {
        match self.0 {
            Repr::Unsigned(v) => w.write_u64(v),
            Repr::Signed(v) => w.write_i64(v),
            Repr::Float(v) => w.write_number_str(Number::float_token(v, &mut [0; MAX_FLOAT_BYTES])),
        }
    }
}

// ---------------------------------------------------------------------------
// BEVE
// ---------------------------------------------------------------------------

impl<'de> beve::Read<'de> for Value {
    fn read<O: Options>(&mut self, r: &mut BeveReader<'de, O>) -> PResult<()> {
        let h = r.head()?;
        *self = read_body(r, h)?;
        Ok(())
    }
}

/// Read a value whose header is already in hand.
///
/// The walk is `transcode::body` building a tree instead of writing text; it
/// recurses and charges depth in the same places, so the two never disagree
/// about what a document holds.
fn read_body<'de, O: Options>(r: &mut BeveReader<'de, O>, h: u8) -> PResult<Value> {
    Ok(match header::ty(h) {
        header::TY_NULL_BOOL => match h {
            header::NULL => Value::Null,
            header::FALSE => Value::Bool(false),
            header::TRUE => Value::Bool(true),
            _ => return Err(ErrorCode::InvalidHeader),
        },
        header::TY_NUMBER => {
            let cat = header::sub(h);
            let code = header::count(h);
            let width = byte_width(cat, code).ok_or(ErrorCode::InvalidHeader)?;
            Value::Number(read_number(cat, code, r.take(width)?)?)
        }
        header::TY_STRING => Value::String(r.str_body()?.to_owned()),
        header::TY_OBJECT => read_object(r, h)?,
        header::TY_TYPED_ARRAY => read_typed_array(r, h)?,
        header::TY_GENERIC_ARRAY => {
            let n = r.count()?;
            r.enter()?;
            // Each element is at least a header byte, so the input bounds the
            // count; `cautious` bounds what that many are allowed to cost.
            let mut items = Vec::with_capacity(cautious::<Value>(n.min(r.remaining())));
            for _ in 0..n {
                let h = r.head()?;
                items.push(read_body(r, h)?);
            }
            r.leave();
            Value::Array(items)
        }
        header::TY_EXTENSION => match header::ext_id(h) {
            header::EXT_COMPLEX => read_complex(r)?,
            header::EXT_MATRIX => read_matrix(r)?,
            _ => return Err(ErrorCode::UnsupportedFeature),
        },
        _ => return Err(ErrorCode::InvalidHeader),
    })
}

/// One number from its little-endian payload, `bytes` being exactly
/// `byte_width(cat, code)` long.
fn read_number(cat: u8, code: u8, bytes: &[u8]) -> PResult<Number> {
    let half = |b: &[u8]| u16::from_le_bytes(b.try_into().expect("2 bytes"));
    match cat {
        header::CAT_FLOAT => {
            let v = match code {
                0 => f64::from(bf16_to_f32(half(bytes))),
                1 => f64::from(f16_to_f32(half(bytes))),
                2 => f64::from(f32::from_le_bytes(bytes.try_into().expect("4 bytes"))),
                3 => f64::from_le_bytes(bytes.try_into().expect("8 bytes")),
                _ => return Err(ErrorCode::UnsupportedFeature),
            };
            // A `Value` has no form for a non-finite float; a declared `f64`
            // field can hold one, so this is the one place the two differ.
            Number::from_f64(v).ok_or(ErrorCode::NumberOutOfRange)
        }
        header::CAT_UNSIGNED => u64::try_from(le_u128(bytes))
            .map(|v| Number(Repr::Unsigned(v)))
            .map_err(|_| ErrorCode::NumberOutOfRange),
        header::CAT_SIGNED => i64::try_from(sign_extend(le_u128(bytes), bytes.len()))
            .map(Number::from)
            .map_err(|_| ErrorCode::NumberOutOfRange),
        _ => Err(ErrorCode::ExpectedNumber),
    }
}

fn read_object<'de, O: Options>(r: &mut BeveReader<'de, O>, h: u8) -> PResult<Value> {
    let cat = header::sub(h);
    let width = key_width(h)?;
    let members = r.count()?;
    r.enter()?;
    let mut map = Object::new();
    for _ in 0..members {
        let key = match cat {
            header::CAT_FLOAT => r.str_body()?.to_owned(),
            // JSON has no key but a string, so an integer key becomes its
            // digits, the form `ToJsonKey` writes one in.
            header::CAT_SIGNED => sign_extend(le_u128(r.take(width)?), width).to_string(),
            _ => le_u128(r.take(width)?).to_string(),
        };
        let h = r.head()?;
        map.insert(key, read_body(r, h)?);
    }
    r.leave();
    Ok(Value::Object(map))
}

fn read_typed_array<'de, O: Options>(r: &mut BeveReader<'de, O>, h: u8) -> PResult<Value> {
    r.enter()?;
    let items = match r.typed_head(h)? {
        Typed::Bools(n) => {
            let payload = r.take(n.div_ceil(8))?;
            (0..n)
                .map(|i| Value::Bool((payload[i >> 3] >> (i & 7)) & 1 == 1))
                .collect()
        }
        Typed::Strings(n) => {
            // Each string is at least its count byte; see the generic array.
            let mut items = Vec::with_capacity(cautious::<Value>(n.min(r.remaining())));
            for _ in 0..n {
                items.push(Value::String(r.str_body()?.to_owned()));
            }
            items
        }
        Typed::Fixed(elem, n) => {
            let cat = header::sub(elem);
            let code = header::count(elem);
            let width = byte_width(cat, code).ok_or(ErrorCode::InvalidHeader)?;
            let payload = r.take(payload_len(elem, n)?)?;
            payload
                .chunks_exact(width)
                .map(|chunk| read_number(cat, code, chunk).map(Value::Number))
                .collect::<PResult<Vec<_>>>()?
        }
    };
    r.leave();
    Ok(Value::Array(items))
}

/// A complex value as `[re, im]`, or an array of them.
fn read_complex<'de, O: Options>(r: &mut BeveReader<'de, O>) -> PResult<Value> {
    let (class, width, pairs) = r.complex_head()?;
    let cat = header::sub(class);
    let code = header::count(class);
    let payload = r.take(complex_payload(width, pairs)?)?;
    let pair = |z: &[u8]| -> PResult<Value> {
        Ok(Value::Array(vec![
            Value::Number(read_number(cat, code, &z[..width])?),
            Value::Number(read_number(cat, code, &z[width..2 * width])?),
        ]))
    };
    match pairs {
        None => pair(payload),
        Some(_) => Ok(Value::Array(
            payload
                .chunks_exact(2 * width)
                .map(pair)
                .collect::<PResult<Vec<_>>>()?,
        )),
    }
}

/// A matrix as the object both formats read one back from.
fn read_matrix<'de, O: Options>(r: &mut BeveReader<'de, O>) -> PResult<Value> {
    let layout = MatrixLayout::from_byte(r.take(1)?[0]).ok_or(ErrorCode::InvalidMatrixLayout)?;
    r.enter()?;
    let mut map = Object::new();
    map.insert(
        "layout".to_owned(),
        Value::String(layout.as_str().to_owned()),
    );
    let h = r.head()?;
    map.insert("extents".to_owned(), read_body(r, h)?);
    let h = r.head()?;
    map.insert("value".to_owned(), read_body(r, h)?);
    r.leave();
    Ok(Value::Object(map))
}

impl beve::Write for Value {
    fn write<O: Options>(&self, w: &mut BeveWriter<'_, O>) {
        match self {
            Value::Null => w.write_null(),
            Value::Bool(b) => w.write_bool(*b),
            Value::Number(n) => n.write(w),
            Value::String(s) => w.write_str(s),
            // Elements of a document carry their own headers: an array of
            // numbers here is a generic array, since nothing promises the
            // next element is a number too.
            Value::Array(items) => w.write_iter(items.len(), items.iter()),
            Value::Object(map) => w.write_keyed(map.len(), map.iter()),
        }
    }

    fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

impl<'de> beve::Read<'de> for Number {
    fn read<O: Options>(&mut self, r: &mut BeveReader<'de, O>) -> PResult<()> {
        let h = r.head()?;
        if header::ty(h) != header::TY_NUMBER {
            return Err(ErrorCode::ExpectedNumber);
        }
        let cat = header::sub(h);
        let code = header::count(h);
        let width = byte_width(cat, code).ok_or(ErrorCode::InvalidHeader)?;
        *self = read_number(cat, code, r.take(width)?)?;
        Ok(())
    }
}

impl beve::Write for Number {
    fn write<O: Options>(&self, w: &mut BeveWriter<'_, O>) {
        match self.0 {
            Repr::Unsigned(v) => v.write(w),
            Repr::Signed(v) => v.write(w),
            Repr::Float(v) => v.write(w),
        }
    }
}

// ---------------------------------------------------------------------------
// The `value!` macro
// ---------------------------------------------------------------------------

/// Build a [`Value`] from JSON-shaped syntax.
///
/// Keys are string literals or identifiers-in-quotes; values are literals,
/// nested `[..]` and `{..}`, or any expression that is `Into<Value>`.
///
/// ```
/// let name = "api";
/// let d = structio::value!({
///     "name": name,
///     "port": 8080,
///     "hosts": ["a", "b"],
///     "tls": null,
/// });
/// assert_eq!(d.to_string(), r#"{"hosts":["a","b"],"name":"api","port":8080,"tls":null}"#);
/// ```
#[macro_export]
macro_rules! value {
    ($($tt:tt)+) => {
        $crate::__value_internal!($($tt)+)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __value_internal {
    // Array: accumulate elements into a Vec<Value>.
    (@array [$($elems:expr,)*]) => {
        vec![$($elems,)*]
    };
    (@array [$($elems:expr),*]) => {
        vec![$($elems),*]
    };
    (@array [$($elems:expr,)*] null $($rest:tt)*) => {
        $crate::__value_internal!(@array [$($elems,)* $crate::__value_internal!(null)] $($rest)*)
    };
    (@array [$($elems:expr,)*] true $($rest:tt)*) => {
        $crate::__value_internal!(@array [$($elems,)* $crate::__value_internal!(true)] $($rest)*)
    };
    (@array [$($elems:expr,)*] false $($rest:tt)*) => {
        $crate::__value_internal!(@array [$($elems,)* $crate::__value_internal!(false)] $($rest)*)
    };
    (@array [$($elems:expr,)*] [$($array:tt)*] $($rest:tt)*) => {
        $crate::__value_internal!(@array [$($elems,)* $crate::__value_internal!([$($array)*])] $($rest)*)
    };
    (@array [$($elems:expr,)*] {$($map:tt)*} $($rest:tt)*) => {
        $crate::__value_internal!(@array [$($elems,)* $crate::__value_internal!({$($map)*})] $($rest)*)
    };
    (@array [$($elems:expr,)*] $next:expr, $($rest:tt)*) => {
        $crate::__value_internal!(@array [$($elems,)* $crate::__value_internal!($next),] $($rest)*)
    };
    (@array [$($elems:expr,)*] $last:expr) => {
        $crate::__value_internal!(@array [$($elems,)* $crate::__value_internal!($last)])
    };
    (@array [$($elems:expr),*] , $($rest:tt)*) => {
        $crate::__value_internal!(@array [$($elems,)*] $($rest)*)
    };
    (@array [$($elems:expr),*] $unexpected:tt $($rest:tt)*) => {
        $crate::__value_unexpected!($unexpected)
    };

    // Object: munch `key: value` pairs into the map `$object`.
    (@object $object:ident () () ()) => {};
    (@object $object:ident [$($key:tt)+] ($value:expr) , $($rest:tt)*) => {
        let _ = $object.insert(($($key)+).into(), $value);
        $crate::__value_internal!(@object $object () ($($rest)*) ($($rest)*));
    };
    (@object $object:ident [$($key:tt)+] ($value:expr) , ) => {
        let _ = $object.insert(($($key)+).into(), $value);
    };
    (@object $object:ident [$($key:tt)+] ($value:expr)) => {
        let _ = $object.insert(($($key)+).into(), $value);
    };
    (@object $object:ident ($($key:tt)+) (: null $($rest:tt)*) $copy:tt) => {
        $crate::__value_internal!(@object $object [$($key)+] ($crate::__value_internal!(null)) $($rest)*);
    };
    (@object $object:ident ($($key:tt)+) (: true $($rest:tt)*) $copy:tt) => {
        $crate::__value_internal!(@object $object [$($key)+] ($crate::__value_internal!(true)) $($rest)*);
    };
    (@object $object:ident ($($key:tt)+) (: false $($rest:tt)*) $copy:tt) => {
        $crate::__value_internal!(@object $object [$($key)+] ($crate::__value_internal!(false)) $($rest)*);
    };
    (@object $object:ident ($($key:tt)+) (: [$($array:tt)*] $($rest:tt)*) $copy:tt) => {
        $crate::__value_internal!(@object $object [$($key)+] ($crate::__value_internal!([$($array)*])) $($rest)*);
    };
    (@object $object:ident ($($key:tt)+) (: {$($map:tt)*} $($rest:tt)*) $copy:tt) => {
        $crate::__value_internal!(@object $object [$($key)+] ($crate::__value_internal!({$($map)*})) $($rest)*);
    };
    (@object $object:ident ($($key:tt)+) (: $value:expr , $($rest:tt)*) $copy:tt) => {
        $crate::__value_internal!(@object $object [$($key)+] ($crate::__value_internal!($value)) , $($rest)*);
    };
    (@object $object:ident ($($key:tt)+) (: $value:expr) $copy:tt) => {
        $crate::__value_internal!(@object $object [$($key)+] ($crate::__value_internal!($value)));
    };
    (@object $object:ident ($($key:tt)+) (:) $copy:tt) => {
        $crate::__value_internal!();
    };
    (@object $object:ident ($($key:tt)+) () $copy:tt) => {
        $crate::__value_internal!();
    };
    (@object $object:ident () (: $($rest:tt)*) ($colon:tt $($copy:tt)*)) => {
        $crate::__value_unexpected!($colon);
    };
    (@object $object:ident ($($key:tt)*) (, $($rest:tt)*) ($comma:tt $($copy:tt)*)) => {
        $crate::__value_unexpected!($comma);
    };
    (@object $object:ident () (($key:expr) : $($rest:tt)*) $copy:tt) => {
        $crate::__value_internal!(@object $object ($key) (: $($rest)*) (: $($rest)*));
    };
    (@object $object:ident ($($key:tt)*) (: $($unexpected:tt)+) $copy:tt) => {
        $crate::__value_expect_expr_comma!($($unexpected)+);
    };
    (@object $object:ident ($($key:tt)*) ($tt:tt $($rest:tt)*) $copy:tt) => {
        $crate::__value_internal!(@object $object ($($key)* $tt) ($($rest)*) ($($rest)*));
    };

    // Values.
    (null) => {
        $crate::Value::Null
    };
    (true) => {
        $crate::Value::Bool(true)
    };
    (false) => {
        $crate::Value::Bool(false)
    };
    ([]) => {
        $crate::Value::Array(::std::vec::Vec::new())
    };
    ([ $($tt:tt)+ ]) => {
        $crate::Value::Array($crate::__value_internal!(@array [] $($tt)+))
    };
    ({}) => {
        $crate::Value::Object($crate::Object::new())
    };
    ({ $($tt:tt)+ }) => {
        $crate::Value::Object({
            let mut object = $crate::Object::new();
            $crate::__value_internal!(@object object () ($($tt)+) ($($tt)+));
            object
        })
    };
    ($other:expr) => {
        $crate::Value::from($other)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __value_unexpected {
    () => {};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __value_expect_expr_comma {
    ($e:expr , $($tt:tt)*) => {};
}
