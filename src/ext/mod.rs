//! The two BEVE extensions that carry data, as ordinary Rust types.
//!
//! BEVE's core covers what JSON covers. Its extensions cover what scientific
//! data needs on top of that: [`Complex`] for a complex number, and [`Matrix`]
//! for an array that knows its own shape. Both are stored compactly, which is
//! the point of them. A run of complex numbers is one header, one count, and
//! the interleaved components, so a `Vec<Complex<f64>>` is a single `memcpy` in
//! each direction, exactly as a `Vec<f64>` is.
//!
//! ```
//! use structio::{Complex, Matrix, MatrixLayout};
//!
//! let signal = vec![Complex::new(1.0f64, 2.0), Complex::new(3.0, -4.0)];
//! let bytes = structio::to_beve(&signal);
//! assert_eq!(structio::from_beve::<Vec<Complex<f64>>>(&bytes).unwrap(), signal);
//!
//! let m = Matrix::new(MatrixLayout::RowMajor, vec![2, 3], (0..6).collect()).unwrap();
//! let bytes = structio::to_beve(&m);
//! assert_eq!(structio::from_beve::<Matrix<i32>>(&bytes).unwrap(), m);
//! ```
//!
//! # They are types, not a numeric library
//!
//! [`Complex`] has two public fields and no arithmetic. It is here so a complex
//! number can be *stored*, and it is deliberately not a competitor to
//! `num-complex`: nothing here would make it a better one, and adding operators
//! would make this crate a numerics dependency for everyone who only wanted to
//! read a file. A program that computes with complex numbers should keep doing
//! so in whatever type it already uses and convert at the edge, which for a
//! `#[repr(C)]` pair is a field-by-field move the optimizer removes.
//!
//! # Both formats, one type
//!
//! [`object!`](crate::object) declares a struct for JSON and BEVE at once, so a
//! field of either type has to work in both. Where BEVE has an extension, JSON
//! gets the encoding it would have had anyway: a complex number is `[re, im]`
//! and a matrix is `{"layout":…,"extents":[…],"value":[…]}`. Those are the
//! forms [`beve_to_json`](crate::beve_to_json) writes as well, and the forms
//! both types still read back from BEVE, so a producer that has no extensions
//! is understood without a second declaration.
//!
//! # What is not here
//!
//! The other two extensions carry no data of their own. The delimiter separates
//! documents in a stream and is handled by [`beve::Documents`](crate::beve::Documents);
//! the type tag is deprecated. Both are still stepped over correctly wherever
//! they appear.

mod complex;
pub(crate) mod matrix;

pub use complex::Complex;
pub use matrix::{Matrix, MatrixLayout, MatrixRef};
