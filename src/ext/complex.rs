//! [`Complex`], and the BEVE extension that stores a run of them.
//!
//! # Why a run is worth an extension
//!
//! A complex number written as an ordinary pair costs two headers and two
//! payloads, and a million of them cost two million headers. The extension
//! states the class once and then interleaves the components, so the payload of
//! a `Vec<Complex<f64>>` is bit for bit the slice's own memory and moves in one
//! copy each way.
//!
//! # The header that is not a header
//!
//! A complex array's elements carry nothing of their own, so driving one
//! element at a time needs a stand-in, the way a typed array's element header
//! is installed rather than read. The obvious stand-in is unusable: a class
//! header of the run form is bit for bit the number header of the same class
//! and width, so installing it would let a `Vec<f64>` bulk-read the components
//! of a complex array as though they were plain numbers.
//! [`header::complex_element`] keeps the class and width and puts the one
//! undefined type code where the type goes, which leaves a byte no document can
//! hold and nothing else can be mistaken for.

use crate::beve;
use crate::beve::header;
use crate::beve::impls::NumericBytes;
use crate::error::{ErrorCode, PResult};
use crate::json;
use crate::options::Options;

/// A complex number: two components of the same type, real part first.
///
/// A plain pair with no arithmetic on it. See the [module docs](super) for why
/// it stops there.
///
/// ```
/// use structio::Complex;
///
/// let z = Complex::new(3.0f64, -4.0);
/// assert_eq!(structio::to_string(&z), "[3,-4]");
/// assert_eq!(structio::from_beve::<Complex<f64>>(&structio::to_beve(&z)).unwrap(), z);
/// ```
///
/// # Which component types
///
/// The BEVE class field names a category and a width, so the components are
/// the fixed-width numbers it can name: `f32`, `f64`, and the signed and
/// unsigned integers from 8 through 128 bits. `usize` and `isize` are absent on
/// purpose, a wire format being the last place to store a width that depends on
/// the machine that wrote it.
///
/// Reading is as lenient about width as every other number here: a
/// `Complex<f64>` reads a document that stored `Complex<f32>`, and a
/// `Complex<i64>` reads one that stored `Complex<u8>`, with the same range
/// check a plain integer field would get.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Complex<T> {
    pub re: T,
    pub im: T,
}

impl<T> Complex<T> {
    #[inline]
    pub const fn new(re: T, im: T) -> Self {
        Complex { re, im }
    }
}

impl<T> From<(T, T)> for Complex<T> {
    #[inline]
    fn from((re, im): (T, T)) -> Self {
        Complex { re, im }
    }
}

impl<T> From<Complex<T>> for (T, T) {
    #[inline]
    fn from(z: Complex<T>) -> Self {
        (z.re, z.im)
    }
}

// ---------------------------------------------------------------------------
// The shared halves
// ---------------------------------------------------------------------------

/// Read one complex number, in whichever of the two forms it was written.
///
/// Generic over the component, since every case that differs between component
/// types is already settled by the time this runs: the extension form hands its
/// components to the ordinary scalar readers under the header
/// [`Reader::complex_form`](beve::Reader::complex_form) reports, and the array
/// form is two values like any other two.
fn read_beve<'de, O: Options, T: beve::Read<'de>>(
    z: &mut Complex<T>,
    r: &mut beve::Reader<'de, O>,
) -> PResult<()> {
    match r.complex_form()? {
        Some(elem) => r.complex_pair(elem, &mut z.re, &mut z.im),
        // A two-element array. Anything that is neither that nor the extension
        // `complex_form` has already refused.
        None => {
            let n = r.read_seq(|r, i| match i {
                0 => beve::Read::read(&mut z.re, r),
                1 => beve::Read::read(&mut z.im, r),
                _ => Err(ErrorCode::ExpectedComplex),
            })?;
            if n == 2 {
                Ok(())
            } else {
                Err(ErrorCode::ExpectedComplex)
            }
        }
    }
}

/// Read one complex number from JSON, which has only the array form.
fn read_json<'de, O: Options, T: json::Read<'de>>(
    z: &mut Complex<T>,
    p: &mut json::Parser<'de, O>,
) -> PResult<()> {
    let n = p.read_seq(|p, i| match i {
        0 => json::Read::read(&mut z.re, p),
        1 => json::Read::read(&mut z.im, p),
        _ => Err(ErrorCode::ExpectedComplex),
    })?;
    if n == 2 {
        Ok(())
    } else {
        Err(ErrorCode::ExpectedComplex)
    }
}

/// Write one complex number as `[re,im]`.
fn write_json<O: Options, T: json::Write>(z: &Complex<T>, w: &mut json::Writer<'_, O>) {
    w.open(b'[');
    w.element(&z.re);
    w.element(&z.im);
    w.close(b']');
}

// ---------------------------------------------------------------------------
// One set of impls per component
// ---------------------------------------------------------------------------

/// Enumerated rather than blanket, for two reasons that apply to different
/// halves of it.
///
/// The BEVE half has no choice. The bulk paths reinterpret `[Complex<T>]` as
/// bytes, which is sound only for a fixed-width number with no padding and no
/// invalid bit pattern, and the class header is a constant per component that
/// no bound could produce. Restricting the set is also simply correct there:
/// BEVE's class field can name these and nothing else, so a `Complex` of
/// anything else has no encoding to be written in.
///
/// The JSON half could be blanket, and is enumerated anyway to keep the two in
/// lockstep. A struct declared with [`object!`](crate::object) gets impls for
/// both formats at once, so a `Complex<String>` that satisfied one and not the
/// other would compile until the field was written as BEVE and then fail on a
/// bound nobody wrote. Failing on the declaration is the better error.
macro_rules! impl_complex {
    ($($t:ty, $cat:expr, $code:expr);* $(;)?) => {$(
        // `#[repr(C)]` promises the field order; these pin the rest of what the
        // reinterpretation needs, at every width, at compile time.
        const _: () = {
            assert!(size_of::<Complex<$t>>() == 2 * size_of::<$t>());
            assert!(align_of::<Complex<$t>>() == align_of::<$t>());
        };

        // SAFETY: two fields of a type that has no padding and whose every bit
        // pattern is a value leave no padding between or after them and add no
        // invalid pattern of their own, which the assertions above confirm at
        // this width. The wire form is the two components in field order, which
        // is what `#[repr(C)]` fixes.
        //
        // The element header is the component's with the type bits swapped for
        // the undefined code, which is what `complex_element` does and why no
        // numeric array can produce it.
        unsafe impl NumericBytes for Complex<$t> {
            const ELEMENT: u8 = header::complex_element(<$t as NumericBytes>::ELEMENT);
        }

        impl<'de> beve::Read<'de> for Complex<$t> {
            #[inline]
            fn read<O: Options>(&mut self, r: &mut beve::Reader<'de, O>) -> PResult<()> {
                read_beve(self, r)
            }

            fn read_bulk<O: Options>(
                out: &mut Vec<Self>,
                n: usize,
                elem: u8,
                r: &mut beve::Reader<'de, O>,
            ) -> PResult<bool> {
                if elem != <Self as NumericBytes>::ELEMENT || cfg!(target_endian = "big") {
                    return Ok(false);
                }
                // The tag test is the half of the contract the bound does not
                // cover: it establishes that the stored class is this one.
                r.read_block(out, n)?;
                Ok(true)
            }
        }

        impl beve::Write for Complex<$t> {
            const ARRAY: Option<&'static [u8]> = Some(&[
                header::COMPLEX,
                header::complex_class($cat, $code, header::COMPLEX_MANY),
            ]);

            #[inline]
            fn write<O: Options>(&self, w: &mut beve::Writer<'_, O>) {
                const CLASS: u8 = header::complex_class($cat, $code, header::COMPLEX_ONE);
                w.write_complex(CLASS, self.re.to_le_bytes(), self.im.to_le_bytes());
            }

            fn write_payload<O: Options>(items: &[Self], w: &mut beve::Writer<'_, O>) {
                if cfg!(target_endian = "little") {
                    w.write_block(items)
                } else {
                    for z in items {
                        w.raw(&z.re.to_le_bytes());
                        w.raw(&z.im.to_le_bytes());
                    }
                }
            }
        }

        impl<'de> json::Read<'de> for Complex<$t> {
            #[inline]
            fn read<O: Options>(&mut self, p: &mut json::Parser<'de, O>) -> PResult<()> {
                read_json(self, p)
            }
        }

        impl json::Write for Complex<$t> {
            #[inline]
            fn write<O: Options>(&self, w: &mut json::Writer<'_, O>) {
                write_json(self, w);
            }
        }
    )*}
}

impl_complex! {
    f32, header::CAT_FLOAT, 2;
    f64, header::CAT_FLOAT, 3;
    i8, header::CAT_SIGNED, header::code_for(1);
    i16, header::CAT_SIGNED, header::code_for(2);
    i32, header::CAT_SIGNED, header::code_for(4);
    i64, header::CAT_SIGNED, header::code_for(8);
    i128, header::CAT_SIGNED, header::code_for(16);
    u8, header::CAT_UNSIGNED, header::code_for(1);
    u16, header::CAT_UNSIGNED, header::code_for(2);
    u32, header::CAT_UNSIGNED, header::code_for(4);
    u64, header::CAT_UNSIGNED, header::code_for(8);
    u128, header::CAT_UNSIGNED, header::code_for(16);
}
