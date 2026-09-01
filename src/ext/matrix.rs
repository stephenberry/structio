//! [`Matrix`], and the BEVE extension that stores its shape beside its data.
//!
//! The wire form is a layout byte, then the extents, then the data, the last
//! two being ordinary values. That is what makes a matrix hold whatever a
//! sequence can hold: a matrix of `f64` stores a typed array, and a matrix of
//! [`Complex`](super::Complex) stores a complex array, with no case here for
//! either.
//!
//! # The shape cannot be wrong
//!
//! Writing is infallible everywhere in this crate, which leaves nowhere to
//! report a matrix whose extents and data disagree. So the type refuses to hold
//! one: the fields are private, [`Matrix::new`] checks the shape, and the only
//! mutation that reaches the data cannot change its length. A failed read
//! resets the matrix rather than leaving it half filled, which is the one place
//! in the crate where that matters, a partially written struct being merely
//! incomplete where a partially written matrix would be a lie.

use crate::beve;
use crate::beve::header;
use crate::error::{ErrorCode, PResult};
use crate::json;
use crate::options::Options;

/// Which index of a matrix varies fastest in storage.
///
/// The names on the wire are `layout_right` and `layout_left`, after
/// `std::mdspan`, and are what this writes; the more common names for the same
/// two orders are what it is spelled with here. Reading accepts either
/// vocabulary, plus the bare `right` and `left`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MatrixLayout {
    /// The rightmost index varies fastest: row major, `layout_right`.
    #[default]
    RowMajor,
    /// The leftmost index varies fastest: column major, `layout_left`.
    ColumnMajor,
}

impl MatrixLayout {
    /// The name this layout is written under, in JSON and in a BEVE object.
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            MatrixLayout::RowMajor => "layout_right",
            MatrixLayout::ColumnMajor => "layout_left",
        }
    }

    /// The byte this layout is written as, inside the matrix extension.
    ///
    /// Not public: [`header::LAYOUT_RIGHT`] and [`header::LAYOUT_LEFT`] already
    /// state the wire encoding, and one commitment to it is enough.
    #[inline]
    pub(crate) const fn as_byte(self) -> u8 {
        match self {
            MatrixLayout::RowMajor => header::LAYOUT_RIGHT,
            MatrixLayout::ColumnMajor => header::LAYOUT_LEFT,
        }
    }

    /// The layout a byte inside the matrix extension names.
    ///
    /// Two values are defined; the rest are refused rather than guessed at,
    /// since a layout read wrongly transposes the data silently.
    #[inline]
    pub(crate) const fn from_byte(b: u8) -> Option<Self> {
        match b {
            header::LAYOUT_RIGHT => Some(MatrixLayout::RowMajor),
            header::LAYOUT_LEFT => Some(MatrixLayout::ColumnMajor),
            _ => None,
        }
    }
}

/// The inverse of [`MatrixLayout::as_str`], and more permissive than it: the
/// writer emits only the two names the specification gives, where this accepts
/// the common spellings of the same two orders as well.
impl core::str::FromStr for MatrixLayout {
    type Err = ErrorCode;

    fn from_str(s: &str) -> Result<Self, ErrorCode> {
        match s {
            "layout_right" | "row_major" | "right" => Ok(MatrixLayout::RowMajor),
            "layout_left" | "column_major" | "left" => Ok(MatrixLayout::ColumnMajor),
            _ => Err(ErrorCode::InvalidMatrixLayout),
        }
    }
}

/// How many elements `extents` describes.
///
/// The product of the dimensions, with one deliberate reading of the empty
/// list: it describes *no* elements, rather than the single one an empty
/// product would give. That is what makes [`Matrix::default`] a legal matrix
/// holding nothing, with no allocation behind it, and it gives up nothing,
/// a rank-zero matrix being something BEVE has no way to store anyway.
///
/// `None` is an overflow, which no matrix that exists can reach and every
/// matrix read off a wire can claim.
fn element_count(extents: &[usize]) -> Option<usize> {
    if extents.is_empty() {
        return Some(0);
    }
    extents
        .iter()
        .copied()
        .try_fold(1usize, |acc, e| acc.checked_mul(e))
}

/// An owned matrix: a layout, the length of each dimension, and the elements
/// in that order.
///
/// ```
/// use structio::{Matrix, MatrixLayout};
///
/// let m = Matrix::new(MatrixLayout::RowMajor, vec![2, 2], vec![1.0f64, 2.0, 3.0, 4.0]).unwrap();
/// assert_eq!(m.extents(), &[2, 2]);
///
/// let bytes = structio::to_beve(&m);
/// assert_eq!(structio::from_beve::<Matrix<f64>>(&bytes).unwrap(), m);
/// assert_eq!(
///     structio::beve_to_json(&bytes).unwrap(),
///     r#"{"layout":"layout_right","extents":[2,2],"value":[1,2,3,4]}"#
/// );
/// ```
///
/// The elements are one flat run, since that is what the format stores and what
/// makes storing it cheap. Indexing into them is arithmetic this type
/// deliberately does not do: which of the two layouts is in force changes the
/// answer, and a program that computes with matrices already has a type that
/// knows. Use [`Matrix::into_parts`] to hand the pieces to it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Matrix<T> {
    layout: MatrixLayout,
    extents: Vec<usize>,
    data: Vec<T>,
}

impl<T> Matrix<T> {
    /// Build a matrix, checking that the extents describe the data.
    ///
    /// The only failure is [`InvalidMatrixShape`](ErrorCode::InvalidMatrixShape),
    /// and it is checked here so that it can never be checked again: nothing
    /// downstream of this, writing included, has to consider a matrix whose
    /// halves disagree.
    pub fn new(layout: MatrixLayout, extents: Vec<usize>, data: Vec<T>) -> Result<Self, ErrorCode> {
        if element_count(&extents) != Some(data.len()) {
            return Err(ErrorCode::InvalidMatrixShape);
        }
        Ok(Matrix {
            layout,
            extents,
            data,
        })
    }

    #[inline]
    pub fn layout(&self) -> MatrixLayout {
        self.layout
    }

    /// Reinterpret the same elements in the other storage order.
    ///
    /// Nothing moves: the layout says how the elements the matrix already holds
    /// are to be read, so changing it changes what they mean.
    #[inline]
    pub fn set_layout(&mut self, layout: MatrixLayout) {
        self.layout = layout;
    }

    /// The length of each dimension, outermost first.
    #[inline]
    pub fn extents(&self) -> &[usize] {
        &self.extents
    }

    /// How many dimensions the matrix has.
    #[inline]
    pub fn rank(&self) -> usize {
        self.extents.len()
    }

    #[inline]
    pub fn data(&self) -> &[T] {
        &self.data
    }

    /// The elements, mutably.
    ///
    /// A slice rather than the `Vec`, which is what keeps the shape true: the
    /// values may change and their number may not.
    #[inline]
    pub fn data_mut(&mut self) -> &mut [T] {
        &mut self.data
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Take the three pieces apart, which is where a matrix stops being this
    /// crate's problem and starts being your linear algebra library's.
    #[inline]
    pub fn into_parts(self) -> (MatrixLayout, Vec<usize>, Vec<T>) {
        (self.layout, self.extents, self.data)
    }
}

/// A borrowed matrix, for writing one whose data you already hold.
///
/// The counterpart of [`Matrix`] in the direction where nothing has to be
/// owned: a buffer that came from somewhere else is written straight out of
/// where it already is.
///
/// There is no reading counterpart. A matrix's data can be pointed at, when
/// the document put it in the aligned form and
/// [`Reader::try_slice`](beve::Reader::try_slice) can take it, but its extents
/// cannot: they are stored at the narrowest width that holds them, so the
/// bytes behind them are almost never a `[usize]`. A matrix read back this way
/// would be borrowed in one half and owned in the other, which is a different
/// type rather than this one used backwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MatrixRef<'a, T> {
    layout: MatrixLayout,
    extents: &'a [usize],
    data: &'a [T],
}

impl<'a, T> MatrixRef<'a, T> {
    /// Borrow a matrix, checking that the extents describe the data.
    ///
    /// There are no accessors, and none would earn their place: the three
    /// pieces reach this type from the caller and stay theirs. What it adds is
    /// the shape check and somewhere for the [`Write`](beve::Write) impls to
    /// hang.
    pub fn new(
        layout: MatrixLayout,
        extents: &'a [usize],
        data: &'a [T],
    ) -> Result<Self, ErrorCode> {
        if element_count(extents) != Some(data.len()) {
            return Err(ErrorCode::InvalidMatrixShape);
        }
        Ok(MatrixRef {
            layout,
            extents,
            data,
        })
    }

    /// Copy into an owned matrix.
    ///
    /// Not `to_owned`: [`MatrixRef`] is [`Copy`], so the blanket [`ToOwned`]
    /// impl already gives it a method of that name returning another
    /// `MatrixRef`, and two methods spelled the same returning different types
    /// is a trap however the resolution falls out.
    pub fn to_matrix(self) -> Matrix<T>
    where
        T: Clone,
    {
        Matrix {
            layout: self.layout,
            extents: self.extents.to_vec(),
            data: self.data.to_vec(),
        }
    }
}

// ---------------------------------------------------------------------------
// The keys both object forms use
// ---------------------------------------------------------------------------

// The bare names are what a reader matches a key against; the quoted forms are
// what a JSON writer puts in front of a value. Both spellings live here, and
// `transcode` reaches for the quoted ones too, so a key cannot be renamed in
// one of the three places and not the others.
const LAYOUT: &str = "layout";
const EXTENTS: &str = "extents";
const VALUE: &str = "value";
pub(crate) const LAYOUT_MEMBER: &str = "\"layout\":";
pub(crate) const EXTENTS_MEMBER: &str = "\"extents\":";
pub(crate) const VALUE_MEMBER: &str = "\"value\":";

/// The three members of the object form, tracked by hand so that
/// [`Options::ERROR_ON_MISSING_KEYS`] reaches a matrix too. Reading one goes
/// through `read_map`, which cannot know that this particular caller's key set
/// is fixed rather than arbitrary.
const SEEN_LAYOUT: u8 = 1;
const SEEN_EXTENTS: u8 = 2;
const SEEN_VALUE: u8 = 4;
const SEEN_ALL: u8 = SEEN_LAYOUT | SEEN_EXTENTS | SEEN_VALUE;

/// The first of the three members an object form left out, where the policy
/// asks for all three, and `None` where nothing is owed.
///
/// The offset can only point at the object, a member that is not there having
/// no position of its own, so the name goes alongside it exactly as a
/// generated reader's does. Declaration order, matching `Fields::missing`, so
/// an `object!` of these same three keys would say the same thing.
fn missing_member<O: Options>(seen: u8) -> Option<&'static str> {
    if !O::ERROR_ON_MISSING_KEYS || seen == SEEN_ALL {
        return None;
    }
    Some(if seen & SEEN_LAYOUT == 0 {
        LAYOUT
    } else if seen & SEEN_EXTENTS == 0 {
        EXTENTS
    } else {
        VALUE
    })
}

// ---------------------------------------------------------------------------
// BEVE
// ---------------------------------------------------------------------------

/// Write the extents as a typed array of unsigned integers, at the narrowest
/// width that holds the largest of them.
///
/// A matrix's dimensions are small numbers standing in front of a payload that
/// is not, so storing them at `usize` width would spend eight bytes each on
/// values that almost always fit in one. Every reader widens on the way in, so
/// the narrowing costs nothing but the bytes it saves.
fn write_extents<O: Options>(w: &mut beve::Writer<'_, O>, extents: &[usize]) {
    let max = extents.iter().copied().max().unwrap_or(0);
    let width = if u8::try_from(max).is_ok() {
        1
    } else if u16::try_from(max).is_ok() {
        2
    } else if u32::try_from(max).is_ok() {
        4
    } else {
        8
    };
    w.begin_typed_array(
        header::array_of(header::CAT_UNSIGNED, header::code_for(width)),
        extents.len(),
    );
    for &e in extents {
        // The low bytes of the little-endian form are the little-endian form of
        // the narrower value, and the width was chosen from the largest extent,
        // so nothing is lost. Explicitly little-endian, so the host is not the
        // question.
        w.raw(&(e as u64).to_le_bytes()[..width]);
    }
}

/// Write a matrix, borrowed or owned, as the extension.
fn write_beve<O: Options, T: beve::Write>(
    layout: MatrixLayout,
    extents: &[usize],
    data: &[T],
    w: &mut beve::Writer<'_, O>,
) {
    w.begin_matrix(layout.as_byte());
    write_extents(w, extents);
    // Whatever array the element type has, which is how a matrix of complex
    // numbers costs no case of its own.
    w.write_slice(data);
}

/// Settle the shape after a read, whatever happened.
///
/// A `Matrix` cannot be *built* with a shape it does not hold, so it must not
/// be left with one either. This is the one moment the three parts are filled
/// independently and could disagree, and the one place in the crate where a
/// partially written value would be worse than an empty one. All three are
/// reset, the layout included: two thirds of a document is not a matrix that
/// was read, and a layout is exactly the field whose being wrong shows up
/// nowhere.
fn commit_shape<T>(m: &mut Matrix<T>, outcome: PResult<()>) -> PResult<()> {
    if outcome.is_ok() && element_count(&m.extents) == Some(m.data.len()) {
        return Ok(());
    }
    m.layout = MatrixLayout::default();
    m.extents.clear();
    m.data.clear();
    outcome?;
    Err(ErrorCode::InvalidMatrixShape)
}

fn read_beve<'de, O: Options, T>(m: &mut Matrix<T>, r: &mut beve::Reader<'de, O>) -> PResult<()>
where
    T: beve::Read<'de> + Default,
{
    let outcome = fill_beve(m, r);
    commit_shape(m, outcome)
}

fn fill_beve<'de, O: Options, T>(m: &mut Matrix<T>, r: &mut beve::Reader<'de, O>) -> PResult<()>
where
    T: beve::Read<'de> + Default,
{
    match r.peek() {
        // The extension, which carries all three of layout, extents and data
        // by construction and so can never be missing one.
        Some(header::MATRIX) => {
            r.head()?;
            m.layout =
                MatrixLayout::from_byte(r.take(1)?[0]).ok_or(ErrorCode::InvalidMatrixLayout)?;
            // One level for the extension, which is exactly what `skip_value`
            // charges it; the two values inside then charge their own.
            r.enter()?;
            beve::Read::read(&mut m.extents, r)?;
            beve::Read::read(&mut m.data, r)?;
            r.leave();
            Ok(())
        }
        // The object form, which is what a producer without the extension
        // writes and what the JSON side always writes.
        Some(h) if header::ty(h) == header::TY_OBJECT => {
            // Where the object begins, so a member it never carried is
            // reported against the object, as a generated reader reports it.
            let open = r.position();
            let mut seen = 0u8;
            r.read_map(|r, key| match key {
                beve::Key::Str(LAYOUT) => {
                    seen |= SEEN_LAYOUT;
                    m.layout = r.read_str()?.parse()?;
                    Ok(())
                }
                beve::Key::Str(EXTENTS) => {
                    seen |= SEEN_EXTENTS;
                    beve::Read::read(&mut m.extents, r)
                }
                beve::Key::Str(VALUE) => {
                    seen |= SEEN_VALUE;
                    beve::Read::read(&mut m.data, r)
                }
                // A member this shape does not name. Three keys is still a
                // schema, so this is an unknown key like any other and the
                // policy decides, by hand here for the reason `missing_member`
                // gives.
                _ if O::ERROR_ON_UNKNOWN_KEYS => Err(ErrorCode::UnknownKey),
                _ => r.skip_value(),
            })?;
            match missing_member::<O>(seen) {
                None => Ok(()),
                Some(key) => {
                    r.rewind(open);
                    r.set_error_key(key);
                    Err(ErrorCode::MissingKey)
                }
            }
        }
        Some(_) => Err(ErrorCode::ExpectedMatrix),
        None => Err(ErrorCode::UnexpectedEnd),
    }
}

impl<'de, T> beve::Read<'de> for Matrix<T>
where
    T: beve::Read<'de> + Default,
{
    fn read<O: Options>(&mut self, r: &mut beve::Reader<'de, O>) -> PResult<()> {
        read_beve(self, r)
    }
}

impl<T: beve::Write> beve::Write for Matrix<T> {
    #[inline]
    fn write<O: Options>(&self, w: &mut beve::Writer<'_, O>) {
        write_beve(self.layout, &self.extents, &self.data, w);
    }
}

impl<T: beve::Write> beve::Write for MatrixRef<'_, T> {
    #[inline]
    fn write<O: Options>(&self, w: &mut beve::Writer<'_, O>) {
        write_beve(self.layout, self.extents, self.data, w);
    }
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// Write a matrix as the object BEVE's own object form uses, so the two
/// encodings differ in compactness and in nothing else.
fn write_json<O: Options, T: json::Write>(
    layout: MatrixLayout,
    extents: &[usize],
    data: &[T],
    w: &mut json::Writer<'_, O>,
) {
    w.open(b'{');
    w.member(LAYOUT_MEMBER, layout.as_str());
    w.member(EXTENTS_MEMBER, extents);
    w.member(VALUE_MEMBER, data);
    w.close(b'}');
}

fn read_json<'de, O: Options, T>(m: &mut Matrix<T>, p: &mut json::Parser<'de, O>) -> PResult<()>
where
    T: json::Read<'de> + Default,
{
    // Where the object begins, so a member it never named is reported against
    // the object, as a generated reader reports it.
    p.skip_ws();
    let open = p.position();
    let mut seen = 0u8;
    let outcome = p.read_map(|p, key| match key.as_str() {
        LAYOUT => {
            seen |= SEEN_LAYOUT;
            let name = p.read_string()?;
            m.layout = name.as_str().parse()?;
            Ok(())
        }
        EXTENTS => {
            seen |= SEEN_EXTENTS;
            json::Read::read(&mut m.extents, p)
        }
        VALUE => {
            seen |= SEEN_VALUE;
            json::Read::read(&mut m.data, p)
        }
        // Unknown, and the policy decides, exactly as on the BEVE side.
        _ if O::ERROR_ON_UNKNOWN_KEYS => Err(ErrorCode::UnknownKey),
        _ => p.skip_value(),
    });
    let outcome = outcome.and_then(|()| match missing_member::<O>(seen) {
        None => Ok(()),
        Some(key) => {
            p.rewind(open);
            p.set_error_key(key);
            Err(ErrorCode::MissingKey)
        }
    });
    commit_shape(m, outcome)
}

impl<'de, T> json::Read<'de> for Matrix<T>
where
    T: json::Read<'de> + Default,
{
    fn read<O: Options>(&mut self, p: &mut json::Parser<'de, O>) -> PResult<()> {
        read_json(self, p)
    }
}

impl<T: json::Write> json::Write for Matrix<T> {
    #[inline]
    fn write<O: Options>(&self, w: &mut json::Writer<'_, O>) {
        write_json(self.layout, &self.extents, &self.data, w);
    }
}

impl<T: json::Write> json::Write for MatrixRef<'_, T> {
    #[inline]
    fn write<O: Options>(&self, w: &mut json::Writer<'_, O>) {
        write_json(self.layout, self.extents, self.data, w);
    }
}
