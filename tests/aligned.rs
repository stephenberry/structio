//! Writing the aligned typed-array form.
//!
//! The form itself was always read, skipped, indexed and framed; what is new is
//! that this crate can write it. So the property that carries this file is that
//! turning it on changes the layout and nothing else: the same documents come
//! back as the same values, through the same readers, whatever offset an array
//! happens to land on.
//!
//! The layout claim is asserted directly rather than through a reader, because
//! a reader that ignored the padding would agree with one that got it right.
//! Every array here is written last in its document, so the payload starts at
//! `len() - elements * width` and its alignment can be read off the bytes.

use structio::beve::{self, Documents, Feed, Mode, Writer, header};
use structio::{
    Complex, ErrorCode, Matrix, MatrixLayout, beve_to_json, from_beve, from_beve_at, to_beve,
    to_beve_aligned, to_string, validate_beve,
};

#[derive(Default, Debug, PartialEq, Clone)]
struct Reading {
    sensor: String,
    ok: bool,
    samples: Vec<f64>,
}
structio::object!(Reading {
    sensor,
    ok,
    samples
});

fn reading() -> Reading {
    Reading {
        sensor: "thermocouple".into(),
        ok: true,
        samples: (0..37).map(|i| i as f64 * 0.5).collect(),
    }
}

/// Where an array's payload begins, given that it is the last thing in `doc`.
fn payload_start<T>(doc: &[u8], elements: usize) -> usize {
    doc.len() - elements * size_of::<T>()
}

// ---------------------------------------------------------------------------
// The form on the wire
// ---------------------------------------------------------------------------

#[test]
fn the_preamble_is_the_marker_the_element_header_the_count_and_the_padding() {
    let doc = to_beve_aligned(&vec![1.0f64, 2.0]);
    #[rustfmt::skip]
    assert_eq!(
        doc,
        [
            0x5c, // the aligned marker
            0x64, // a typed array of eight-byte floats
            0x08, // two elements
            0x04, // four bytes of padding, which lands the payload on 8
            0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0xf0, 0x3f, // 1.0
            0, 0, 0, 0, 0, 0, 0x00, 0x40, // 2.0
        ]
    );
    assert_eq!(from_beve::<Vec<f64>>(&doc).unwrap(), vec![1.0, 2.0]);
}

/// Write `values` at every offset a preamble in front of it can produce, and
/// require the payload to land on its element width each time.
fn lands_on_its_width<T>(values: Vec<T>)
where
    T: beve::Write + for<'de> beve::Read<'de> + Default + Clone + PartialEq + std::fmt::Debug,
{
    for shift in 0..40 {
        let doc = to_beve_aligned(&(vec![0u8; shift], values.clone()));
        let start = payload_start::<T>(&doc, values.len());
        assert_eq!(
            start % size_of::<T>(),
            0,
            "{} bytes wide, shifted by {shift}: payload at {start}",
            size_of::<T>()
        );
        let (_, back) = from_beve::<(Vec<u8>, Vec<T>)>(&doc).unwrap();
        assert_eq!(back, values);
    }
}

#[test]
fn the_payload_lands_on_a_multiple_of_its_element_width() {
    lands_on_its_width(vec![1u16, 2, 3]);
    lands_on_its_width(vec![1.5f32, 2.5]);
    lands_on_its_width(vec![-1i32, 2, 3, 4, 5]);
    lands_on_its_width(vec![1.0f64, 2.0, 3.0]);
    lands_on_its_width(vec![i128::MIN, i128::MAX]);
    lands_on_its_width(vec![7usize; 9]);
}

#[test]
fn an_array_with_no_elements_is_padded_the_same_way() {
    // Nothing to align, but the form is the form: a reader that computes a
    // pointer before it looks at the count must still be handed one.
    for shift in 0..40 {
        let doc = to_beve_aligned(&(vec![0u8; shift], Vec::<f64>::new()));
        assert_eq!(payload_start::<f64>(&doc, 0) % 8, 0);
        let (_, back) = from_beve::<(Vec<u8>, Vec<f64>)>(&doc).unwrap();
        assert!(back.is_empty());
    }
}

#[test]
fn only_numbers_wider_than_a_byte_change() {
    // Booleans and strings have no aligned form, one-byte elements are aligned
    // wherever they land, and a scalar is not an array at all.
    assert_eq!(
        to_beve(&vec![true, false]),
        to_beve_aligned(&vec![true, false])
    );
    assert_eq!(
        to_beve(&vec!["a".to_string(), "b".to_string()]),
        to_beve_aligned(&vec!["a".to_string(), "b".to_string()])
    );
    assert_eq!(to_beve(&vec![1u8, 2, 3]), to_beve_aligned(&vec![1u8, 2, 3]));
    assert_eq!(to_beve(&vec![-1i8, 2]), to_beve_aligned(&vec![-1i8, 2]));
    assert_eq!(to_beve(&1.0f64), to_beve_aligned(&1.0f64));
    assert_eq!(to_beve(&"text"), to_beve_aligned(&"text"));
}

#[test]
fn a_complex_array_keeps_the_extension_form() {
    // The specification gives the aligned form to numeric typed arrays. A run
    // of complex numbers is neither, so it is written exactly as before.
    let signal = vec![Complex::new(1.0f64, 2.0), Complex::new(3.0, -4.0)];
    assert_eq!(to_beve(&signal), to_beve_aligned(&signal));
    assert_eq!(
        from_beve::<Vec<Complex<f64>>>(&to_beve_aligned(&signal)).unwrap(),
        signal
    );
}

// ---------------------------------------------------------------------------
// The same documents, read the same ways
// ---------------------------------------------------------------------------

#[test]
fn an_element_read_widens_from_the_padded_payload() {
    // The bulk path takes the payload whole; a stored width the destination
    // does not share walks it element by element, from the same place.
    let doc = to_beve_aligned(&(vec![0u8; 3], vec![1u16, 2, 3]));
    let (_, back) = from_beve::<(Vec<u8>, Vec<u64>)>(&doc).unwrap();
    assert_eq!(back, vec![1u64, 2, 3]);
}

#[test]
fn a_pointer_indexes_into_a_padded_payload() {
    let value = reading();
    let doc = to_beve_aligned(&value);
    for (i, want) in value.samples.iter().enumerate() {
        let at = format!("/samples/{i}");
        assert_eq!(from_beve_at::<f64>(&doc, &at).unwrap(), *want);
    }
}

#[test]
fn it_validates_and_transcodes_to_the_json_the_typed_writer_would_produce() {
    let value = reading();
    let doc = to_beve_aligned(&value);
    assert_eq!(payload_start::<f64>(&doc, value.samples.len()) % 8, 0);
    assert_eq!(from_beve::<Reading>(&doc).unwrap(), value);
    validate_beve(&doc).unwrap();
    assert_eq!(beve_to_json(&doc).unwrap(), to_string(&value));
}

#[test]
fn it_frames_as_one_value_however_the_bytes_arrive() {
    let doc = to_beve_aligned(&reading());
    let mut feed = Feed::new(Mode::Values);
    let mut got = Vec::new();
    for &b in &doc {
        feed.push(&[b]);
        while let Some(v) = feed.next_value::<Reading>() {
            got.push(v.unwrap());
        }
    }
    feed.end();
    assert_eq!(got, vec![reading()]);
}

#[test]
fn a_stream_hands_out_its_elements_one_at_a_time() {
    let samples: Vec<f64> = (0..64).map(|i| i as f64).collect();
    let doc = to_beve_aligned(&samples);
    let mut docs = Documents::array(&doc[..]);
    let got: Vec<f64> = docs.iter::<f64>().map(Result::unwrap).collect();
    assert_eq!(got, samples);
}

#[derive(Default, Debug, PartialEq, Clone)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}
structio::array!(Vec3 [f64; x, y, z]);

#[test]
fn a_struct_stored_as_a_typed_array_is_padded_like_one() {
    // Naming an element type says the struct is stored the way a run of that
    // type is, which has to keep holding once the run is padded.
    let v = Vec3 {
        x: 1.5,
        y: -2.0,
        z: 3.25,
    };
    assert_eq!(to_beve_aligned(&v), to_beve_aligned(&[1.5f64, -2.0, 3.25]));
    for shift in 0..24 {
        let doc = to_beve_aligned(&(vec![0u8; shift], v.clone()));
        assert_eq!(payload_start::<f64>(&doc, 3) % 8, 0, "shifted by {shift}");
        assert_eq!(from_beve::<(Vec<u8>, Vec3)>(&doc).unwrap().1, v);
    }
}

#[test]
fn the_block_is_still_taken_in_one_copy() {
    // The form exists so that a reader can take the payload whole. A reader
    // that declined it would still be correct and would read the fastest thing
    // in the format the slowest way there is, one element at a time, so this
    // asks the bulk path directly rather than inferring it from a value.
    let samples = vec![1.5f64; 1000];
    for doc in [to_beve(&samples), to_beve_aligned(&samples)] {
        let mut r = beve::Reader::new(&doc);
        let mut out: Vec<f64> = Vec::new();
        assert!(r.try_bulk(&mut out).unwrap(), "declined the bulk path");
        assert_eq!(out, samples);
    }
}

#[test]
fn a_bool_array_is_not_an_aligned_one_however_its_bytes_read() {
    // Booleans, strings and the aligned form share a category and are told
    // apart by the byte-count field alone. The bulk path has to check it: this
    // document is a bool array whose following bytes happen to spell an f64
    // array's aligned preamble, and taking those at face value would hand back
    // two numbers that are not in the document at all.
    let mut doc = vec![header::BOOL_ARRAY, 0x64, 0x08, 0x00];
    doc.extend_from_slice(&1.0f64.to_le_bytes());
    doc.extend_from_slice(&2.0f64.to_le_bytes());
    assert_eq!(
        from_beve::<Vec<f64>>(&doc).unwrap_err().code,
        ErrorCode::ExpectedNumber
    );
}

/// Bits rather than values: no NaN equals itself, and the payload is exactly
/// what is in question here.
fn bits32(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}

fn bits64(v: &[f64]) -> Vec<u64> {
    v.iter().map(|x| x.to_bits()).collect()
}

#[test]
fn a_float_keeps_its_exact_bits_in_either_form() {
    // A signalling NaN is what separates taking the payload whole from
    // converting element by element: the conversion quiets it.
    let f32s: Vec<f32> = [
        0x7f80_0001u32,
        0x7fc0_1234,
        0xffc0_0001,
        0x0000_0001,
        0x7f80_0000,
    ]
    .iter()
    .map(|&b| f32::from_bits(b))
    .collect();
    let f64s: Vec<f64> = [
        0x7ff0_0000_0000_0001u64,
        0xfff8_0000_dead_beef,
        0x0000_0000_0000_0001,
    ]
    .iter()
    .map(|&b| f64::from_bits(b))
    .collect();
    for doc in [to_beve(&f32s), to_beve_aligned(&f32s)] {
        assert_eq!(bits32(&from_beve::<Vec<f32>>(&doc).unwrap()), bits32(&f32s));
    }
    for doc in [to_beve(&f64s), to_beve_aligned(&f64s)] {
        assert_eq!(bits64(&from_beve::<Vec<f64>>(&doc).unwrap()), bits64(&f64s));
    }
    // A field reads one number rather than a block, and the two must not
    // disagree about what came off the wire.
    for &v in &f32s {
        assert_eq!(
            from_beve::<f32>(&to_beve(&v)).unwrap().to_bits(),
            v.to_bits()
        );
    }
    for &v in &f64s {
        assert_eq!(
            from_beve::<f64>(&to_beve(&v)).unwrap().to_bits(),
            v.to_bits()
        );
    }
}

// ---------------------------------------------------------------------------
// Matrices
// ---------------------------------------------------------------------------

#[test]
fn a_matrix_pads_the_data_it_holds() {
    let m = Matrix::new(
        MatrixLayout::RowMajor,
        vec![2, 3],
        (0..6).map(f64::from).collect(),
    )
    .unwrap();
    let doc = to_beve_aligned(&m);
    assert_eq!(payload_start::<f64>(&doc, 6) % 8, 0);
    assert_eq!(from_beve::<Matrix<f64>>(&doc).unwrap(), m);
    validate_beve(&doc).unwrap();
    assert_eq!(beve_to_json(&doc).unwrap(), to_string(&m));
}

#[test]
fn a_matrix_of_complex_numbers_is_unchanged() {
    let m = Matrix::new(
        MatrixLayout::ColumnMajor,
        vec![1, 2],
        vec![Complex::new(1.0f64, 2.0), Complex::new(3.0, -4.0)],
    )
    .unwrap();
    // Its extents are one byte wide and its data is the complex extension, so
    // there is nothing in it with an aligned form.
    assert_eq!(to_beve(&m), to_beve_aligned(&m));
}

#[test]
fn wide_extents_are_padded_like_any_other_numbers() {
    let m = Matrix::new(MatrixLayout::RowMajor, vec![70_000, 1], vec![0u8; 70_000]).unwrap();
    let doc = to_beve_aligned(&m);
    assert_eq!(from_beve::<Matrix<u8>>(&doc).unwrap(), m);
    // The extension header, the layout byte, then the extents: the aligned
    // marker, a typed array of four-byte unsigned integers, a count of two,
    // and the padding that lands the two extents on a multiple of four.
    assert_eq!(&doc[..4], &[0x16, 0x00, 0x5c, 0x54]);
    let padding = doc[5] as usize;
    assert_eq!((6 + padding) % 4, 0);
}

// ---------------------------------------------------------------------------
// The offset is the document's, not the buffer's
// ---------------------------------------------------------------------------

#[test]
fn a_sink_writer_pads_against_where_the_document_started() {
    // The buffer holds only the tail once it has drained, so a writer that
    // measured padding from the buffer would go wrong as soon as it did.
    let value = reading();
    let want = to_beve_aligned(&value);
    for capacity in [1usize, 2, 3, 7, 8, 16, 17, 64, 4096] {
        let mut out = Vec::new();
        let mut w =
            Writer::<structio::Standard>::to_sink_with_capacity(&mut out, capacity).aligned();
        beve::Write::write(&value, &mut w);
        w.finish().unwrap();
        assert_eq!(out, want, "capacity {capacity}");
    }
}

#[test]
fn a_block_handed_straight_to_the_sink_is_still_counted() {
    // A payload at least as large as the whole buffer bypasses it, and the
    // arrays after it still have to be padded against the right offset.
    let value = (vec![0u8; 4096], vec![1.0f64; 512], vec![2.0f32; 3]);
    let want = to_beve_aligned(&value);
    let mut out = Vec::new();
    let mut w = Writer::<structio::Standard>::to_sink_with_capacity(&mut out, 64).aligned();
    beve::Write::write(&value, &mut w);
    w.finish().unwrap();
    assert_eq!(out, want);
    assert_eq!(payload_start::<f32>(&want, 3) % 4, 0);
    assert_eq!(
        from_beve::<(Vec<u8>, Vec<f64>, Vec<f32>)>(&out).unwrap(),
        value
    );
}
