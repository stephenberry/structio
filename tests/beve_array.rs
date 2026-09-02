//! Reading a document that is one numeric array straight out of an `io::Read`.
//!
//! The property doing most of the work here is the same one the rest of the
//! streaming tests rest on: this must agree with `from_beve`. It is a second
//! path to the same values, taken for what it does *not* hold rather than for
//! any difference in what it produces, so anywhere the two disagree the fast
//! path is simply wrong. That is asserted over every numeric type, both array
//! forms, and a reader that hands back one byte at a time.
//!
//! What it declines is the other half. The call exists to move a block without
//! touching an element, so a stored width that would need converting is an
//! error rather than a quiet fall back to the slow path, and the streaming
//! reader is where that case goes.

use std::io;

use structio::beve::header;
use structio::{Complex, ErrorCode, StreamError};

/// A reader that hands back one byte per call, so every internal `read_exact`
/// has to loop. A real stream behind a decompressor is free to do this.
struct Dribble<'a>(&'a [u8]);

impl io::Read for Dribble<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.0.is_empty() || out.is_empty() {
            return Ok(0);
        }
        out[0] = self.0[0];
        self.0 = &self.0[1..];
        Ok(1)
    }
}

fn code(e: &StreamError) -> ErrorCode {
    e.as_parse()
        .unwrap_or_else(|| panic!("expected a parse failure, got {e}"))
        .code
}

// ---------------------------------------------------------------------------
// Agreement with the batch reader
// ---------------------------------------------------------------------------

/// Every numeric type, both array forms, whole and dribbled.
macro_rules! check {
    ($($t:ty),* $(,)?) => {$({
        for len in [0usize, 1, 2, 7, 64, 1000] {
            let values: Vec<$t> = (0..len).map(|i| i as $t).collect();
            for doc in [structio::to_beve(&values), structio::to_beve_aligned(&values)] {
                let whole: Vec<$t> = structio::from_beve_reader_array(&doc[..]).unwrap();
                assert_eq!(whole, values, "{} len {len}", stringify!($t));

                // Same bytes, arriving one at a time.
                let dribbled: Vec<$t> =
                    structio::from_beve_reader_array(Dribble(&doc)).unwrap();
                assert_eq!(dribbled, values, "{} len {len} dribbled", stringify!($t));

                // And the same as the reader that holds the document.
                assert_eq!(
                    structio::from_beve::<Vec<$t>>(&doc).unwrap(),
                    values,
                    "{} len {len} batch",
                    stringify!($t)
                );
            }
        }
    })*};
}

#[test]
fn it_agrees_with_the_batch_reader_for_every_numeric_type() {
    check!(
        u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize, f32, f64
    );
}

/// Floats carry values a `0..len` cast cannot reach.
#[test]
fn awkward_float_payloads_survive() {
    let values = vec![
        0.0f64,
        -0.0,
        f64::MIN,
        f64::MAX,
        f64::EPSILON,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ];
    let doc = structio::to_beve(&values);
    let got: Vec<f64> = structio::from_beve_reader_array(&doc[..]).unwrap();
    assert_eq!(got, values);

    // NaN separately, having no equality.
    let doc = structio::to_beve(&vec![f64::NAN]);
    let got: Vec<f64> = structio::from_beve_reader_array(&doc[..]).unwrap();
    assert!(got[0].is_nan());
}

/// The payload is taken a megabyte at a time, so an array larger than one
/// exercises the seam between chunks.
#[test]
#[cfg_attr(miri, ignore)]
fn an_array_longer_than_one_chunk_reads_whole() {
    // Comfortably past the 1 MiB the reader takes per pass.
    let values: Vec<f64> = (0..400_000).map(|i| i as f64 * 0.5).collect();
    let doc = structio::to_beve(&values);
    assert!(doc.len() > 3 * 1024 * 1024);

    let got: Vec<f64> = structio::from_beve_reader_array(&doc[..]).unwrap();
    assert_eq!(got, values);
}

// ---------------------------------------------------------------------------
// The `_into` form
// ---------------------------------------------------------------------------

/// Reading into a vector you keep is what makes a pull loop cost one array
/// rather than two: the old contents go, the allocation stays.
#[test]
fn reading_into_a_vector_reuses_its_allocation() {
    let first = structio::to_beve(&(0..1000).map(|i| i as f64).collect::<Vec<f64>>());
    let second = structio::to_beve(&vec![9.5f64; 100]);

    let mut out: Vec<f64> = Vec::new();
    structio::read_beve_array_into(&mut out, &first[..]).unwrap();
    assert_eq!(out.len(), 1000);
    let (buffer, capacity) = (out.as_ptr(), out.capacity());

    structio::read_beve_array_into(&mut out, &second[..]).unwrap();
    assert_eq!(out, vec![9.5f64; 100]);
    assert_eq!(out.as_ptr(), buffer, "the buffer moved");
    assert_eq!(out.capacity(), capacity, "the buffer was resized");
}

/// A failure leaves nothing behind to be mistaken for a short read.
#[test]
fn a_failed_read_empties_the_destination() {
    let doc = structio::to_beve(&(0..1000).map(|i| i as f64).collect::<Vec<f64>>());

    let mut out: Vec<f64> = vec![1.0, 2.0, 3.0];
    let err = structio::read_beve_array_into(&mut out, &doc[..doc.len() - 8]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::UnexpectedEnd);
    assert!(out.is_empty());
}

// ---------------------------------------------------------------------------
// What it declines
// ---------------------------------------------------------------------------

/// A stored width that would have to be converted is refused rather than
/// silently becoming the element-by-element path this call exists to skip.
#[test]
fn a_different_element_type_is_an_error_not_a_conversion() {
    let doc = structio::to_beve(&vec![1.0f32, 2.0, 3.0]);

    // Widening `f32` into `f64` is what the batch reader does happily.
    assert_eq!(
        structio::from_beve::<Vec<f64>>(&doc).unwrap(),
        [1.0f64, 2.0, 3.0]
    );
    let err = structio::from_beve_reader_array::<f64, _>(&doc[..]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::ElementTypeMismatch);

    // Signedness is part of the element type too.
    let doc = structio::to_beve(&vec![1i32, 2, 3]);
    let err = structio::from_beve_reader_array::<u32, _>(&doc[..]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::ElementTypeMismatch);

    // And the streaming reader is where that case goes, converting as it goes.
    let doc = structio::to_beve(&vec![1.0f32, 2.0, 3.0]);
    let mut docs = structio::beve::Documents::array(&doc[..]);
    let got: Vec<f64> = docs.iter::<f64>().map(Result::unwrap).collect();
    assert_eq!(got, [1.0, 2.0, 3.0]);
}

/// The arrays BEVE stores under the same outer category but not as a numeric
/// block: booleans are packed one per bit and strings carry their own lengths.
#[test]
fn the_other_typed_arrays_are_not_numeric_blocks() {
    for doc in [
        structio::to_beve(&vec![true, false, true]),
        structio::to_beve(&vec!["a".to_string(), "b".into()]),
    ] {
        let err = structio::from_beve_reader_array::<u8, _>(&doc[..]).unwrap_err();
        assert_eq!(code(&err), ErrorCode::ElementTypeMismatch, "{doc:02x?}");
    }
}

/// A document that is not an array at all.
#[test]
fn a_value_that_is_not_an_array_says_so() {
    #[derive(Default)]
    struct Rec {
        samples: Vec<f64>,
    }
    structio::object!(Rec { samples });

    for doc in [
        structio::to_beve(&7u32),
        structio::to_beve(&"text"),
        structio::to_beve(&Rec {
            samples: vec![1.0, 2.0],
        }),
        structio::to_beve(&vec![vec![1.0f64], vec![2.0]]),
    ] {
        let err = structio::from_beve_reader_array::<f64, _>(&doc[..]).unwrap_err();
        assert_eq!(code(&err), ErrorCode::ExpectedArray, "{doc:02x?}");
    }

    // An empty stream is not a document, unlike a stream of zero values.
    let err = structio::from_beve_reader_array::<f64, _>(&[][..]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::UnexpectedEnd);
}

/// The array has to be the whole document, as it does for `beve_slice_ref`.
#[test]
fn bytes_after_the_array_are_trailing_content() {
    let mut doc = structio::to_beve(&vec![1.0f64, 2.0, 3.0]);
    doc.extend_from_slice(&structio::to_beve(&7u8));

    let err = structio::from_beve_reader_array::<f64, _>(&doc[..]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::TrailingContent);
}

/// Every truncation of a real document fails, and none of them panics or
/// hands back a short array as though it were whole.
#[test]
fn every_truncation_fails() {
    let values: Vec<f64> = (0..64).map(|i| i as f64).collect();
    for doc in [
        structio::to_beve(&values),
        structio::to_beve_aligned(&values),
    ] {
        for cut in 0..doc.len() {
            let err = structio::from_beve_reader_array::<f64, _>(&doc[..cut]).unwrap_err();
            assert_eq!(code(&err), ErrorCode::UnexpectedEnd, "cut at {cut}");
        }
    }
}

/// A count is read before the payload and need not describe it. Nothing may be
/// reserved on its word alone.
///
/// The bytes here claim four billion `f64`, which is 32 GiB, and deliver none.
/// The test passing at all is most of the point: reserving up front would try
/// to take that much before noticing the stream was empty. What it does take is
/// bounded in `tests/memory.rs`.
#[test]
fn a_count_the_stream_does_not_deliver_fails_rather_than_reserving() {
    let mut doc = vec![header::array_of(header::CAT_FLOAT, 3)];
    let mut size = [0u8; 8];
    let used = header::encode_size(4_000_000_000, &mut size);
    doc.extend_from_slice(&size[..used]);

    let err = structio::from_beve_reader_array::<f64, _>(&doc[..]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::UnexpectedEnd);

    // The same claim with a handful of elements behind it, so the failure is
    // after some payload rather than before any.
    doc.extend_from_slice(&[0u8; 64]);
    let err = structio::from_beve_reader_array::<f64, _>(&doc[..]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::UnexpectedEnd);
}

/// The aligned form's padding is stated in the byte before it, and any amount
/// of it has to be stepped over.
#[test]
fn the_aligned_form_reads_at_every_offset() {
    let values: Vec<f64> = (0..16).map(|i| i as f64).collect();

    // `append_aligned` pads relative to what is already there, so a prefix of
    // each length puts the payload at a different distance from the header.
    for prefix in 0..24usize {
        let mut doc = vec![0u8; prefix];
        structio::append_beve_aligned(&values, &mut doc);

        let got: Vec<f64> = structio::from_beve_reader_array(&doc[prefix..]).unwrap();
        assert_eq!(got, values, "prefix {prefix}");
    }
}

/// An I/O failure is reported as one, not turned into a claim about the bytes.
#[test]
fn an_io_failure_stays_an_io_failure() {
    struct Broken;
    impl io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("no"))
        }
    }

    let err = structio::from_beve_reader_array::<f64, _>(Broken).unwrap_err();
    assert!(err.as_io().is_some(), "{err}");
}

/// A malformed header is one, and says so, rather than being reported as the
/// caller having asked for the wrong type.
///
/// The distinction is not cosmetic: `ElementTypeMismatch` sends the caller to
/// `Documents::array`, and these are documents that reader rejects too. So the
/// code has to be the one the batch reader gives for the same bytes.
#[test]
fn a_malformed_header_is_not_reported_as_the_wrong_element_type() {
    let mut bad: Vec<Vec<u8>> = vec![
        // A numeric width the format does not define, with an honest count of
        // zero behind it so the batch reader reaches the width too.
        vec![header::array_of(header::CAT_FLOAT, 7), 0],
        vec![header::array_of(header::CAT_SIGNED, 6), 0],
        // The one category whose byte counts are enumerated, past the last.
        vec![header::array_of(header::CAT_OTHER, 5), 0],
    ];
    // The aligned form, whose inner header is subject to the same rules.
    for inner in [
        header::number(header::CAT_FLOAT, 3),
        header::array_of(header::CAT_FLOAT, 7),
        header::ALIGNED_ARRAY,
    ] {
        bad.push(vec![header::ALIGNED_ARRAY, inner, 0, 0]);
    }

    for doc in bad {
        let err = structio::from_beve_reader_array::<f64, _>(&doc[..]).unwrap_err();
        assert_eq!(code(&err), ErrorCode::InvalidHeader, "{doc:02x?}");
        assert_eq!(
            structio::from_beve::<Vec<f64>>(&doc).unwrap_err().code,
            ErrorCode::InvalidHeader,
            "{doc:02x?} disagrees with the batch reader"
        );
    }
}

/// The aligned form is subject to the element type rule like any other, its
/// element type being stated one header further in.
#[test]
fn the_aligned_form_checks_its_element_type_too() {
    let doc = structio::to_beve_aligned(&vec![1.0f32, 2.0, 3.0]);
    let err = structio::from_beve_reader_array::<f64, _>(&doc[..]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::ElementTypeMismatch);

    let doc = structio::to_beve_aligned(&vec![1u16, 2, 3]);
    let err = structio::from_beve_reader_array::<i16, _>(&doc[..]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::ElementTypeMismatch);
}

/// A generic array of numbers is not a block.
///
/// `from_beve` reads one into a `Vec<f64>` happily, each element carrying its
/// own header. There is no run of bytes here to move at once, so this is not
/// the shape at all rather than a block of the wrong element type.
#[test]
fn a_generic_array_of_numbers_is_not_this_shape() {
    // A sequence whose elements are not all one width is written generically.
    let doc = structio::to_beve(&(1.0f64, 2u8));
    assert_eq!(structio::from_beve::<Vec<f64>>(&doc).unwrap(), [1.0, 2.0]);

    let err = structio::from_beve_reader_array::<f64, _>(&doc[..]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::ExpectedArray);

    // And the streaming reader is where it goes, as for a width mismatch.
    let mut docs = structio::beve::Documents::array(&doc[..]);
    let got: Vec<f64> = docs.iter::<f64>().map(Result::unwrap).collect();
    assert_eq!(got, [1.0, 2.0]);
}

/// The destination is emptied whichever way the read fails, not only when the
/// payload runs out.
#[test]
fn trailing_content_empties_the_destination_too() {
    let mut doc = structio::to_beve(&vec![1.0f64, 2.0, 3.0]);
    doc.extend_from_slice(&structio::to_beve(&7u8));

    let mut out: Vec<f64> = vec![9.0; 4];
    let err = structio::read_beve_array_into(&mut out, &doc[..]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::TrailingContent);
    assert!(out.is_empty());
}

// ---------------------------------------------------------------------------
// Complex arrays
// ---------------------------------------------------------------------------

/// The complex array is a block like any other, and this call reads it.
///
/// It is an *extension* rather than a typed array, so its tag is a different
/// byte and the preamble carries a class where a typed array carries its size
/// directly. Everything past that is the same read: the payload is interleaved
/// `(re, im)` components, which is the in-memory form of `[Complex<T>]`.
///
/// This is the property the call exists for, and a complex array is where it is
/// worth the most: a buffer of IQ samples is the thing a consumer cannot afford
/// to hold twice.
#[test]
fn a_complex_array_reads_as_a_block() {
    macro_rules! check_complex {
        ($($t:ty),* $(,)?) => {$({
            for len in [0usize, 1, 2, 7, 64, 1000] {
                // `im` differs from `re` so a stride that transposed the two
                // would be visible, and stays in range for the unsigned types.
                let values: Vec<Complex<$t>> = (0..len)
                    .map(|i| Complex { re: i as $t, im: (i + 1) as $t })
                    .collect();
                for doc in [structio::to_beve(&values), structio::to_beve_aligned(&values)] {
                    let whole: Vec<Complex<$t>> =
                        structio::from_beve_reader_array(&doc[..]).unwrap();
                    assert_eq!(whole, values, "Complex<{}> len {len}", stringify!($t));

                    // Same bytes, arriving one at a time.
                    let dribbled: Vec<Complex<$t>> =
                        structio::from_beve_reader_array(Dribble(&doc)).unwrap();
                    assert_eq!(
                        dribbled, values,
                        "Complex<{}> len {len} dribbled", stringify!($t)
                    );

                    // And the same as the reader that holds the document,
                    // which is the agreement the rest of this file rests on.
                    assert_eq!(
                        structio::from_beve::<Vec<Complex<$t>>>(&doc).unwrap(),
                        values,
                        "Complex<{}> len {len} batch",
                        stringify!($t)
                    );
                }
            }
        })*};
    }

    check_complex!(f32, f64, i8, i16, i32, i64, u8, u16, u32, u64);
}

/// A component's byte order is its own.
///
/// The conversion a big-endian host makes reverses each *component*, not each
/// element: a `Complex<f32>` is eight bytes and reversing all eight would
/// transpose `re` and `im` as well as swapping the bytes of each. Asserted
/// through values that are not palindromes in either half, so a stride taken
/// from the element rather than the component is visible in the result on the
/// host where it would be wrong.
#[test]
fn each_component_keeps_its_own_byte_order() {
    let values = vec![
        Complex {
            re: 1.0f64,
            im: 2.0,
        },
        Complex {
            re: f64::MIN,
            im: f64::MAX,
        },
        Complex {
            re: -0.0,
            im: f64::EPSILON,
        },
        Complex {
            re: f64::INFINITY,
            im: f64::NEG_INFINITY,
        },
    ];
    let doc = structio::to_beve(&values);

    let streamed: Vec<Complex<f64>> = structio::from_beve_reader_array(&doc[..]).unwrap();
    assert_eq!(streamed, values);
    assert_eq!(
        streamed,
        structio::from_beve::<Vec<Complex<f64>>>(&doc).unwrap()
    );
}

/// A complex array and a numeric array of its component type are different
/// documents, and neither reads as the other.
///
/// This is what the synthetic element header buys: `complex_element` puts
/// `TY_UNDEFINED` where a number header puts its type, so the two cannot
/// collide and the element check refuses the pairing in both directions
/// without anyone having to ask which form a header came from.
#[test]
fn a_complex_array_is_not_a_numeric_array_of_its_components() {
    let complex = structio::to_beve(&vec![Complex {
        re: 1.0f32,
        im: 2.0,
    }]);
    let numeric = structio::to_beve(&vec![1.0f32, 2.0]);

    let err = structio::from_beve_reader_array::<f32, _>(&complex[..]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::ElementTypeMismatch);

    let err = structio::from_beve_reader_array::<Complex<f32>, _>(&numeric[..]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::ElementTypeMismatch);

    // And a component width that does not match is refused the same way a
    // numeric one is, rather than reinterpreting the payload.
    let err = structio::from_beve_reader_array::<Complex<f64>, _>(&complex[..]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::ElementTypeMismatch);
}

/// The class byte's low three bits are a *form*, and only one of the two
/// defined ones is an array.
///
/// `COMPLEX_ONE` is a lone value with no count before its payload, so reading
/// it as an array would take the first components for a size. The other six
/// values are undefined, and a reader must refuse rather than guess, because
/// the two defined forms differ by exactly that size.
#[test]
fn only_the_run_form_of_a_complex_value_is_an_array() {
    let lone = structio::to_beve(&Complex {
        re: 1.0f64,
        im: 2.0,
    });
    let err = structio::from_beve_reader_array::<Complex<f64>, _>(&lone[..]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::InvalidHeader);

    // Every undefined form, and an undefined component width alongside them.
    let mut doc = structio::to_beve(&vec![Complex {
        re: 1.0f64,
        im: 2.0,
    }]);
    for form in [2u8, 3, 4, 5, 6, 7] {
        doc[1] = header::complex_class(header::CAT_FLOAT, 3, form);
        let err = structio::from_beve_reader_array::<Complex<f64>, _>(&doc[..]).unwrap_err();
        assert_eq!(code(&err), ErrorCode::InvalidHeader, "form {form}");
    }
    doc[1] = header::complex_class(header::CAT_OTHER, 3, header::COMPLEX_MANY);
    let err = structio::from_beve_reader_array::<Complex<f64>, _>(&doc[..]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::InvalidHeader);
}

/// A truncated or over-long complex document fails where a numeric one does,
/// and empties the destination on the way out.
#[test]
fn a_complex_array_is_bounded_like_a_numeric_one() {
    let values: Vec<Complex<f64>> = (0..8)
        .map(|i| Complex {
            re: i as f64,
            im: 0.0,
        })
        .collect();
    let doc = structio::to_beve(&values);

    let mut out: Vec<Complex<f64>> = vec![Complex { re: 9.0, im: 9.0 }; 4];
    let err = structio::read_beve_array_into(&mut out, &doc[..doc.len() - 8]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::UnexpectedEnd);
    assert!(out.is_empty());

    let mut over = doc.clone();
    over.extend_from_slice(&structio::to_beve(&7u8));
    let mut out: Vec<Complex<f64>> = vec![Complex { re: 9.0, im: 9.0 }; 4];
    let err = structio::read_beve_array_into(&mut out, &over[..]).unwrap_err();
    assert_eq!(code(&err), ErrorCode::TrailingContent);
    assert!(out.is_empty());
}
