//! Handing a block of a document back as a slice that points into it.
//!
//! This is what the aligned form `to_beve_aligned` writes is for. The form pads a
//! typed array's payload to a multiple of the element width counted from the
//! start of the document, so a document that itself begins on such an address
//! holds blocks that a `&[f64]` can point at, and reading one costs nothing at
//! all.
//!
//! Whether that second condition holds is a property of the allocation rather
//! than of the bytes, and `Vec<u8>` promises only one byte of alignment: real
//! allocators give more, Miri gives exactly what was asked for. So every test
//! here places its document at an address it chose, which is also the only way
//! to assert the negative cases without waiting for an allocator to disagree.

use std::borrow::Cow;
use structio::beve::{self, header};
use structio::{Complex, ErrorCode, from_beve, read_beve_into, to_beve, to_beve_aligned};

/// The widest element any test here borrows.
const ALIGN: usize = 16;

/// Run `f` on a copy of `doc` placed `shift` bytes past an address that every
/// element width here divides.
fn placed(doc: &[u8], shift: usize, f: impl FnOnce(&[u8])) {
    let mut buf = vec![0u8; doc.len() + ALIGN + shift];
    let base = buf.as_ptr().align_offset(ALIGN) + shift;
    buf[base..base + doc.len()].copy_from_slice(doc);
    f(&buf[base..base + doc.len()]);
}

/// The common case: a document whose first byte is where a mapped file's would
/// be.
fn aligned(doc: &[u8], f: impl FnOnce(&[u8])) {
    placed(doc, 0, f);
}

// ---------------------------------------------------------------------------
// The borrow itself
// ---------------------------------------------------------------------------

#[test]
fn a_block_comes_back_pointing_into_the_document() {
    let values: Vec<f64> = (0..64).map(|i| i as f64 * 1.5).collect();
    let doc = to_beve_aligned(&values);
    aligned(&doc, |doc| {
        let mut r = beve::Reader::new(doc);
        let block = r.try_slice::<f64>().expect("declined an aligned block");
        assert_eq!(block, values.as_slice());
        // Every array here is written last in its document, so the payload is
        // the tail of it. Pointing at that tail is what makes this a borrow
        // rather than a copy that happens to be equal.
        let start = doc.len() - values.len() * size_of::<f64>();
        assert_eq!(block.as_ptr().cast::<u8>(), doc[start..].as_ptr());
        // And the whole value was consumed, padding included.
        r.finish().unwrap();
    });
}

#[test]
fn a_borrow_needs_the_document_to_land_on_the_width_as_well() {
    let values = vec![1.0f64, 2.0, 3.0];
    let doc = to_beve_aligned(&values);
    for shift in 0..ALIGN {
        placed(&doc, shift, |doc| {
            let mut r = beve::Reader::new(doc);
            let taken = r.try_slice::<f64>();
            assert_eq!(taken.is_some(), shift % 8 == 0, "at a shift of {shift}");
            // Whichever it was, the same values come back.
            match taken {
                Some(block) => assert_eq!(block, values.as_slice()),
                None => assert_eq!(from_beve::<Vec<f64>>(doc).unwrap(), values),
            }
        });
    }
}

/// The empty block is the one worth stating: a zero-length borrow is still a
/// pointer, and the aligned form pads for it exactly as for any other, so
/// nothing here may take a shortcut past the address test on the strength of
/// there being nothing to read.
#[test]
fn every_length_borrows_or_copies_to_the_same_values() {
    for len in 0..17 {
        let values: Vec<f64> = (0..len).map(|i| i as f64 - 3.5).collect();
        let doc = to_beve_aligned(&values);
        for shift in [0, 4, 8, 12] {
            placed(&doc, shift, |doc| {
                let mut r = beve::Reader::new(doc);
                match r.try_slice::<f64>() {
                    Some(block) => {
                        assert_eq!(block, values.as_slice(), "{len} at {shift}");
                        r.finish().unwrap();
                    }
                    None => assert_ne!(shift % 8, 0, "declined {len} at {shift}"),
                }
                assert_eq!(from_beve::<Cow<[f64]>>(doc).unwrap().as_ref(), values);
            });
        }
    }
}

#[test]
fn declining_consumes_nothing() {
    let doc = to_beve(&vec![1.0f64, 2.0]);
    aligned(&doc, |doc| {
        let mut r = beve::Reader::new(doc);
        // The plain form puts its payload two bytes in, so it lands on an
        // address no `&[f64]` can point at.
        assert!(r.try_slice::<f64>().is_none());
        assert_eq!(r.position(), 0);
        let mut values: Vec<f64> = Vec::new();
        r.read(&mut values).unwrap();
        assert_eq!(values, [1.0, 2.0]);
        r.finish().unwrap();
    });
}

#[test]
fn a_run_of_bytes_has_no_address_to_satisfy() {
    let values: Vec<u8> = (0..40).collect();
    let doc = to_beve(&values);
    for shift in 0..8 {
        placed(&doc, shift, |doc| {
            let mut r = beve::Reader::new(doc);
            assert_eq!(r.try_slice::<u8>().unwrap(), values.as_slice());
        });
    }
}

// ---------------------------------------------------------------------------
// What is not this type's block
// ---------------------------------------------------------------------------

#[test]
fn a_stored_width_that_is_not_this_ones_declines() {
    let doc = to_beve_aligned(&vec![1.0f32, 2.0]);
    aligned(&doc, |doc| {
        let mut r = beve::Reader::new(doc);
        // Widening is a conversion, and a conversion is a copy.
        assert!(r.try_slice::<f64>().is_none());
        assert_eq!(r.try_slice::<f32>().unwrap(), [1.0f32, 2.0]);
        // Which the ordinary path still does, borrow or no borrow.
        assert_eq!(from_beve::<Cow<[f64]>>(doc).unwrap().as_ref(), [1.0, 2.0]);
    });
}

#[test]
fn a_stored_category_that_is_not_this_ones_declines() {
    let doc = to_beve_aligned(&vec![1u32, 2, 3, 4]);
    aligned(&doc, |doc| {
        let mut r = beve::Reader::new(doc);
        assert!(r.try_slice::<i32>().is_none());
        assert!(r.try_slice::<f32>().is_none());
        assert_eq!(r.try_slice::<u32>().unwrap(), [1u32, 2, 3, 4]);
    });
}

#[test]
fn the_arrays_with_no_block_at_all_decline() {
    for doc in [
        to_beve(&vec![true, false, true]),
        to_beve(&vec!["ab", "cd"]),
    ] {
        aligned(&doc, |doc| {
            let mut r = beve::Reader::new(doc);
            assert!(r.try_slice::<u8>().is_none());
            assert_eq!(r.position(), 0);
        });
    }
}

/// The three `CAT_OTHER` arrays are told apart by their byte-count field
/// alone, so a boolean array whose payload happens to be numbers must not be
/// mistaken for the aligned form. Borrowing it would hand back numbers that
/// are not in the document.
#[test]
fn a_bool_array_is_not_an_aligned_one_however_its_bytes_read() {
    let mut doc = vec![header::BOOL_ARRAY, 0x64, 0x08, 0x00];
    doc.extend_from_slice(&1.0f64.to_le_bytes());
    doc.extend_from_slice(&2.0f64.to_le_bytes());
    for shift in 0..8 {
        placed(&doc, shift, |doc| {
            let mut r = beve::Reader::new(doc);
            assert!(r.try_slice::<f64>().is_none());
        });
    }
}

#[test]
fn a_payload_the_document_does_not_hold_declines() {
    let mut doc = to_beve_aligned(&vec![1.0f64, 2.0, 3.0]);
    doc.truncate(doc.len() - 4);
    aligned(&doc, |doc| {
        let mut r = beve::Reader::new(doc);
        assert!(r.try_slice::<f64>().is_none());
        assert_eq!(r.position(), 0);
        // And the ordinary path is left to say what is wrong with it.
        assert_eq!(
            from_beve::<Vec<f64>>(doc).unwrap_err().code,
            ErrorCode::UnexpectedEnd
        );
    });
}

// ---------------------------------------------------------------------------
// A run of complex numbers, which is a block like any other
// ---------------------------------------------------------------------------

#[test]
fn a_complex_run_is_a_block_too() {
    let values = vec![Complex::new(1.0f64, 2.0), Complex::new(3.0, -4.0)];
    let doc = to_beve(&values);
    // The extension header, the class header and a one-byte count, so the
    // pairs begin three bytes in and land on eight five bytes past an address
    // that eight divides.
    placed(&doc, 5, |doc| {
        let mut r = beve::Reader::new(doc);
        assert_eq!(r.try_slice::<Complex<f64>>().unwrap(), values.as_slice());
        r.finish().unwrap();
    });
    aligned(&doc, |doc| {
        let mut r = beve::Reader::new(doc);
        assert!(r.try_slice::<Complex<f64>>().is_none());
        assert_eq!(from_beve::<Vec<Complex<f64>>>(doc).unwrap(), values);
    });
}

/// The extension has two forms and only one of them is a run. A lone complex
/// number carries no count, so nothing may read it as a block of no elements:
/// a sequence that met one would come back empty rather than refusing it.
#[test]
fn a_lone_complex_number_is_not_a_run_of_none() {
    let doc = to_beve(&Complex::new(1.0f64, 2.0));
    for shift in 0..ALIGN {
        placed(&doc, shift, |doc| {
            let mut r = beve::Reader::new(doc);
            assert!(r.try_slice::<Complex<f64>>().is_none());
            assert_eq!(
                from_beve::<Vec<Complex<f64>>>(doc).unwrap_err().code,
                ErrorCode::ExpectedArray
            );
        });
    }
}

// ---------------------------------------------------------------------------
// `Cow`, which is how a field gets the borrow
// ---------------------------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Trace<'a> {
    sensor: &'a str,
    samples: Cow<'a, [f64]>,
}
structio::object!(['de] Trace<'de> { sensor, samples });

#[test]
fn a_field_borrows_its_block_when_the_document_allows_it() {
    let samples: Vec<f64> = (0..100).map(|i| i as f64 / 8.0).collect();
    let trace = Trace {
        sensor: "thermocouple",
        samples: Cow::Borrowed(&samples),
    };
    for (doc, borrowed) in [(to_beve_aligned(&trace), true), (to_beve(&trace), false)] {
        aligned(&doc, |doc| {
            let held = from_beve::<Trace>(doc).unwrap();
            assert_eq!(held, trace);
            assert_eq!(matches!(held.samples, Cow::Borrowed(_)), borrowed);
        });
    }
}

#[test]
fn a_cow_that_has_to_copy_keeps_the_buffer_it_had() {
    let doc = to_beve(&vec![1.0f64, 2.0, 3.0]);
    aligned(&doc, |doc| {
        let mut held: Cow<[f64]> = Cow::Owned(Vec::with_capacity(64));
        let Cow::Owned(before) = &held else {
            unreachable!()
        };
        let before = before.as_ptr();
        read_beve_into(&mut held, doc).unwrap();
        assert_eq!(held.as_ref(), [1.0, 2.0, 3.0]);
        match held {
            Cow::Owned(after) => assert_eq!(after.as_ptr(), before, "reallocated"),
            Cow::Borrowed(_) => panic!("borrowed the plain form"),
        }
    });
}

#[test]
fn a_cow_reads_json_as_the_owned_half() {
    let held: Cow<[f64]> = structio::from_str("[1,2,3]").unwrap();
    assert!(matches!(held, Cow::Owned(_)));
    assert_eq!(held.as_ref(), [1.0, 2.0, 3.0]);
    assert_eq!(structio::to_string(&held), "[1,2,3]");
    assert_eq!(to_beve(&held), to_beve(&vec![1.0f64, 2.0, 3.0]));
}

// ---------------------------------------------------------------------------
// The borrow as a whole-document call
// ---------------------------------------------------------------------------

#[test]
fn a_document_that_is_one_array_borrows_without_a_reader() {
    let values: Vec<f64> = (0..64).map(|i| i as f64 * 1.5).collect();
    let doc = to_beve_aligned(&values);
    aligned(&doc, |doc| {
        let block = structio::beve_slice_ref::<f64>(doc).expect("declined an aligned document");
        assert_eq!(block, values.as_slice());
        // The same bytes the cursor form hands back, not a copy equal to them.
        let mut r = beve::Reader::new(doc);
        assert_eq!(block.as_ptr(), r.try_slice::<f64>().unwrap().as_ptr());
    });
}

#[test]
fn the_array_has_to_be_the_whole_document() {
    let values: Vec<f64> = (0..8).map(|i| i as f64).collect();
    let mut doc = to_beve_aligned(&values);
    // A second value behind the first: two documents, not one.
    doc.extend_from_slice(&to_beve(&7u8));
    aligned(&doc, |doc| {
        // The cursor form reads the array it is pointed at and says nothing
        // about what follows, which is the difference between the two.
        assert!(beve::Reader::new(doc).try_slice::<f64>().is_some());
        assert!(structio::beve_slice_ref::<f64>(doc).is_none());
        // And the copying read agrees about why.
        assert_eq!(
            from_beve::<Vec<f64>>(doc).unwrap_err().code,
            ErrorCode::TrailingContent
        );
    });
}

#[test]
fn the_whole_document_form_declines_for_the_reasons_the_cursor_one_does() {
    let values: Vec<f64> = (0..8).map(|i| i as f64).collect();
    let doc = to_beve_aligned(&values);
    aligned(&doc, |doc| {
        // Not this element type. Width leniency is a conversion, so a copy.
        assert!(structio::beve_slice_ref::<f32>(doc).is_none());
        assert!(structio::beve_slice_ref::<u64>(doc).is_none());
        assert_eq!(
            from_beve::<Vec<f32>>(doc).unwrap(),
            [0.0f32, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]
        );
    });
    // Not on an address `&[f64]` can point at, whatever the document says.
    placed(&doc, 4, |doc| {
        assert!(structio::beve_slice_ref::<f64>(doc).is_none());
        assert_eq!(from_beve::<Vec<f64>>(doc).unwrap(), values);
    });
}

#[test]
fn an_array_inside_a_document_is_reached_by_seeking_to_it() {
    let values: Vec<f64> = (0..8).map(|i| i as f64).collect();
    // `{"data": [...]}`, the array padded against the start of the document.
    let mut doc = Vec::new();
    structio::append_beve_aligned(
        &std::collections::BTreeMap::from([("data", &values)]),
        &mut doc,
    );
    aligned(&doc, |doc| {
        // Not a bare array, so the whole-document form has nothing to hand out.
        assert!(structio::beve_slice_ref::<f64>(doc).is_none());
        let mut r = beve::Reader::new(doc);
        r.seek("/data").unwrap();
        assert_eq!(
            r.try_slice::<f64>().expect("declined a padded field"),
            values.as_slice()
        );
    });
}
