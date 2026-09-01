//! The two BEVE extensions that carry data: complex numbers and matrices.
//!
//! Most of what is checked here is agreement rather than output. A complex
//! array and a numeric array of the same class are one bit apart on the wire,
//! and a matrix whose extents disagree with its data is a document nothing
//! should ever have written, so the tests that matter are the ones that pin
//! those two boundaries rather than the ones that read a value back.

use structio::beve::header;
use structio::beve::reader::MAX_DEPTH;
use structio::{
    Complex, ErrorCode, Matrix, MatrixLayout, MatrixRef, SkipUnknown, beve, beve_to_json,
    from_beve, from_beve_with, from_str, to_beve, to_string, validate_beve,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A compressed size, for the small counts the documents here hold.
fn size(n: u64) -> Vec<u8> {
    assert!(n < 64, "one-byte sizes only");
    vec![(n as u8) << 2]
}

/// A BEVE object of `members`, each a key and an already-encoded value.
fn object(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = vec![header::OBJECT];
    out.extend(size(members.len() as u64));
    for (key, value) in members {
        out.extend(size(key.len() as u64));
        out.extend_from_slice(key.as_bytes());
        out.extend_from_slice(value);
    }
    out
}

/// `inner` inside `n` generic arrays of one element each.
fn wrap(n: usize, inner: &[u8]) -> Vec<u8> {
    let mut out = inner.to_vec();
    for _ in 0..n {
        let mut next = vec![header::GENERIC_ARRAY];
        next.extend(size(1));
        next.extend_from_slice(&out);
        out = next;
    }
    out
}

/// A value skipped rather than decoded, so a span is judged whole by the same
/// walk a reader uses and by nothing else.
#[derive(Default, Debug)]
struct Any;

impl<'de> beve::Read<'de> for Any {
    fn read<O: structio::Options>(
        &mut self,
        r: &mut beve::Reader<'de, O>,
    ) -> Result<(), ErrorCode> {
        r.skip_value()
    }
}

#[derive(Default, Debug, PartialEq)]
struct Two {
    a: u32,
    b: u32,
}
structio::object!(Two { a, b });

// ---------------------------------------------------------------------------
// Complex: the bytes
// ---------------------------------------------------------------------------

#[test]
fn a_lone_complex_is_the_extension_header_a_class_and_two_components() {
    // Written out rather than computed, so a change to the header helpers
    // cannot move the goalposts along with the code. 0x1E is the complex
    // extension; 0x60 puts the byte count 3 and the class 0 (float) where a
    // number header puts them, with the low three bits saying "one".
    let mut want = vec![0x1E, 0x60];
    want.extend_from_slice(&3.0f64.to_le_bytes());
    want.extend_from_slice(&(-4.0f64).to_le_bytes());
    assert_eq!(to_beve(&Complex::new(3.0f64, -4.0)), want);
}

#[test]
fn a_run_of_complex_numbers_is_one_header_and_one_block() {
    // The same class header with the low bits saying "many", then a count, then
    // the interleaved components. Two numbers cost 2 + 1 + 32 bytes here and
    // would cost 36 as a generic array of pairs.
    let run = vec![Complex::new(1.0f64, 2.0), Complex::new(3.0, 4.0)];
    let mut want = vec![0x1E, 0x61];
    want.extend(size(2));
    for v in [1.0f64, 2.0, 3.0, 4.0] {
        want.extend_from_slice(&v.to_le_bytes());
    }
    assert_eq!(to_beve(&run), want);
    assert_eq!(from_beve::<Vec<Complex<f64>>>(&want).unwrap(), run);
    assert!(validate_beve(&want).is_ok());

    // The bulk copy is a throughput change and must not be a format change, so
    // the block a slice writes has to be the components laid end to end. At a
    // second width, since the copy is where a width could go wrong.
    let narrow = vec![Complex::new(-1.0f32, 0.5), Complex::new(2.0, -0.25)];
    let mut want = vec![
        header::COMPLEX,
        header::complex_class(header::CAT_FLOAT, 2, header::COMPLEX_MANY),
    ];
    want.extend(size(2));
    for z in &narrow {
        want.extend_from_slice(&z.re.to_le_bytes());
        want.extend_from_slice(&z.im.to_le_bytes());
    }
    assert_eq!(to_beve(&narrow), want);
}

#[test]
fn every_component_type_round_trips_through_both_formats() {
    macro_rules! check {
        ($($t:ty, $re:expr, $im:expr);* $(;)?) => {$({
            let z: Complex<$t> = Complex::new($re, $im);
            let run = vec![z, Complex::new($im, $re)];

            assert_eq!(from_beve::<Complex<$t>>(&to_beve(&z)).unwrap(), z);
            assert_eq!(from_beve::<Vec<Complex<$t>>>(&to_beve(&run)).unwrap(), run);
            assert_eq!(from_str::<Complex<$t>>(&to_string(&z)).unwrap(), z);
            assert_eq!(from_str::<Vec<Complex<$t>>>(&to_string(&run)).unwrap(), run);

            // The JSON form is what a transcode of the BEVE form produces, in
            // both the lone and the run shapes.
            assert_eq!(beve_to_json(&to_beve(&z)).unwrap(), to_string(&z));
            assert_eq!(beve_to_json(&to_beve(&run)).unwrap(), to_string(&run));
            assert!(validate_beve(&to_beve(&run)).is_ok());
        })*}
    }
    check! {
        f32, 1.5, -2.5;
        f64, 1.5, -2.5;
        i8, -1, 2; i16, -300, 4; i32, -70_000, 6; i64, -5_000_000_000, 8;
        i128, i128::MIN, i128::MAX;
        u8, 1, 2; u16, 300, 4; u32, 70_000, 6; u64, 5_000_000_000, 8;
        u128, u128::MAX, 0;
    }
}

#[test]
fn an_empty_run_is_still_a_run() {
    let none: Vec<Complex<f64>> = Vec::new();
    let bytes = to_beve(&none);
    assert_eq!(bytes, [&[0x1E, 0x61][..], &size(0)].concat());
    assert_eq!(from_beve::<Vec<Complex<f64>>>(&bytes).unwrap(), none);
    assert_eq!(beve_to_json(&bytes).unwrap(), "[]");
}

// ---------------------------------------------------------------------------
// Complex: what it will and will not read
// ---------------------------------------------------------------------------

#[test]
fn a_complex_array_is_not_confusable_with_a_numeric_one() {
    // This is the whole reason the bulk path uses a synthetic element header. A
    // complex array's class byte is bit for bit the number header of the same
    // class and width -- 0x61 is both -- so a bulk path that matched on it
    // would let a `Vec<f64>` take the components of a complex array as plain
    // numbers, silently and at full speed.
    let run = vec![Complex::new(1.0f64, 2.0), Complex::new(3.0, 4.0)];
    let complex = to_beve(&run);
    let plain = to_beve(&vec![1.0f64, 2.0, 3.0, 4.0]);
    assert_eq!(complex[1], header::number(header::CAT_FLOAT, 3));
    assert_eq!(
        complex[1],
        header::element_of(plain[0]),
        "a complex array's class byte is the element header of the numeric \
         array of the same width, which is the collision the synthetic one avoids"
    );

    assert_eq!(
        from_beve::<Vec<f64>>(&complex).unwrap_err().code,
        ErrorCode::ExpectedNumber
    );
    assert_eq!(
        from_beve::<Vec<Complex<f64>>>(&plain).unwrap_err().code,
        ErrorCode::ExpectedComplex
    );

    // The same at the narrowest width, where the class byte's own type field is
    // zero and so the most likely to be mistaken for something.
    let bytes = to_beve(&vec![Complex::new(1i8, 2)]);
    assert_eq!(
        from_beve::<Vec<Complex<i8>>>(&bytes).unwrap(),
        vec![Complex::new(1i8, 2)]
    );
    assert_eq!(
        from_beve::<Vec<i8>>(&bytes).unwrap_err().code,
        ErrorCode::ExpectedNumber
    );

    // No synthetic element header is a header any document can hold, at any
    // width. That is what makes the byte safe to install without anything
    // downstream having to ask where it came from.
    for width in 0..=4 {
        for cat in [header::CAT_FLOAT, header::CAT_SIGNED, header::CAT_UNSIGNED] {
            let elem =
                header::complex_element(header::complex_class(cat, width, header::COMPLEX_MANY));
            assert_eq!(header::ty(elem), header::TY_UNDEFINED, "{cat}/{width}");
            assert!(
                from_beve::<Any>(&[elem]).is_err(),
                "{elem:#04x} is a value some document could hold"
            );
        }
    }
}

#[test]
fn an_element_of_a_complex_array_is_a_pair_to_every_reader_of_it() {
    // The synthetic element header carries the one type code BEVE leaves
    // undefined, so it is equal to no header any document can hold. What still
    // has to hold is that the readers which meet an element through the
    // ordinary element machinery treat it as the pair it stands for, and that
    // the same byte read out of the *input* is refused.
    let run = to_beve(&vec![Complex::new(1u8, 2), Complex::new(3, 4)]);
    let elem = header::complex_element(run[1]);
    assert_eq!(header::ty(elem), header::TY_UNDEFINED);

    // Stepping over the elements consumes exactly the run and no more.
    assert_eq!(from_beve::<Vec<Any>>(&run).map(|v| v.len()), Ok(2));
    // A type that carries its own header is not an element of one.
    assert_eq!(
        from_beve::<Vec<Matrix<u8>>>(&run).unwrap_err().code,
        ErrorCode::ExpectedMatrix
    );
    // And the byte is not a value: found in the input it is refused, where
    // every code the specification does define would have been read as one.
    assert_eq!(
        from_beve::<Any>(&[elem]).unwrap_err().code,
        ErrorCode::InvalidHeader
    );

    // An extension is not addressable, so a pointer names nothing inside one.
    assert_eq!(
        structio::from_beve_at::<Complex<u8>>(&run, "/1")
            .unwrap_err()
            .code,
        ErrorCode::NoSuchValue
    );
}

#[test]
fn a_stored_width_that_is_not_the_targets_widens_element_by_element() {
    // The bulk path declines and the ordinary one takes over, which is the
    // same leniency every other number here gets.
    let narrow = to_beve(&vec![Complex::new(1.0f32, -2.0), Complex::new(3.0, -4.0)]);
    assert_eq!(
        from_beve::<Vec<Complex<f64>>>(&narrow).unwrap(),
        vec![Complex::new(1.0f64, -2.0), Complex::new(3.0, -4.0)]
    );

    let small = to_beve(&Complex::new(7u8, 8));
    assert_eq!(
        from_beve::<Complex<i64>>(&small).unwrap(),
        Complex::new(7i64, 8)
    );

    // And a value that does not fit is a range error, not a truncation.
    let big = to_beve(&Complex::new(0u64, u64::MAX));
    assert_eq!(
        from_beve::<Complex<u8>>(&big).unwrap_err().code,
        ErrorCode::NumberOutOfRange
    );
}

#[test]
fn a_two_element_array_reads_as_a_complex_number() {
    // The form a producer without the extension writes, and the only form JSON
    // has. Both array kinds are accepted, since a reader never gets to say
    // which one a producer chose.
    let generic = {
        let mut out = vec![header::GENERIC_ARRAY];
        out.extend(size(2));
        out.extend(to_beve(&1.5f64));
        out.extend(to_beve(&-2.5f64));
        out
    };
    assert_eq!(
        from_beve::<Complex<f64>>(&generic).unwrap(),
        Complex::new(1.5, -2.5)
    );
    assert_eq!(
        from_beve::<Complex<f64>>(&to_beve(&vec![1.5f64, -2.5])).unwrap(),
        Complex::new(1.5, -2.5)
    );
    assert_eq!(
        from_str::<Complex<f64>>("[1.5,-2.5]").unwrap(),
        Complex::new(1.5, -2.5)
    );
}

#[test]
fn anything_that_is_not_a_pair_is_refused() {
    for (name, bytes) in [
        ("a number", to_beve(&1.0f64)),
        ("a string", to_beve("1+2i")),
        ("an object", to_beve(&Two { a: 1, b: 2 })),
        ("a run of one", to_beve(&vec![Complex::new(1.0f64, 2.0)])),
    ] {
        assert_eq!(
            from_beve::<Complex<f64>>(&bytes).unwrap_err().code,
            ErrorCode::ExpectedComplex,
            "reading {name} as a complex number"
        );
    }

    // An array of the wrong length is a pair that is not one.
    for wrong in [vec![1.0f64], vec![1.0, 2.0, 3.0]] {
        assert_eq!(
            from_beve::<Complex<f64>>(&to_beve(&wrong))
                .unwrap_err()
                .code,
            ErrorCode::ExpectedComplex
        );
        assert_eq!(
            from_str::<Complex<f64>>(&to_string(&wrong))
                .unwrap_err()
                .code,
            ErrorCode::ExpectedComplex
        );
    }
}

#[test]
fn a_complex_member_that_is_not_wanted_is_stepped_over() {
    for value in [
        to_beve(&Complex::new(1.0f64, 2.0)),
        to_beve(&vec![Complex::new(1u16, 2), Complex::new(3, 4)]),
    ] {
        let doc = object(&[("a", to_beve(&1u32)), ("z", value), ("b", to_beve(&2u32))]);
        assert_eq!(
            from_beve_with::<SkipUnknown, Two>(&doc).unwrap(),
            Two { a: 1, b: 2 }
        );
        assert!(validate_beve(&doc).is_ok());
    }
}

#[test]
fn a_run_of_complex_numbers_costs_no_nesting_level() {
    // A complex array holds numbers and nothing else, so no walk over one ever
    // recurses and none of them charges a level. What matters is that they all
    // make the same choice: charging it in one place and not another is how a
    // validator comes to pass, at the very last level, a document the reader
    // then refuses.
    // Stored narrower than the target, so the bulk path declines and the run is
    // driven element by element. That is the path `read_seq` charges on, and a
    // document read through the bulk copy would never reach it.
    let inner = to_beve(&vec![Complex::new(1.0f32, 2.0), Complex::new(3.0, 4.0)]);
    for wrappers in [
        MAX_DEPTH as usize - 1,
        MAX_DEPTH as usize,
        MAX_DEPTH as usize + 1,
    ] {
        let doc = wrap(wrappers, &inner);
        let valid = validate_beve(&doc).is_ok();
        assert_eq!(valid, wrappers <= MAX_DEPTH as usize, "{wrappers} wrappers");
        assert_eq!(
            valid,
            read_nested::<Vec<Complex<f64>>>(&doc, wrappers).is_ok(),
            "{wrappers} wrappers"
        );
        assert_eq!(valid, from_beve::<Any>(&doc).is_ok(), "{wrappers} wrappers");
    }

    // A typed array does charge one, which is what makes the comparison sharp:
    // the same nesting that fits a complex array does not fit that.
    let doc = wrap(MAX_DEPTH as usize, &to_beve(&vec![1.0f64, 2.0]));
    assert_eq!(
        validate_beve(&doc).unwrap_err().code,
        ErrorCode::ExceededMaxDepth
    );
}

/// Drive `depth` nested arrays with the reader's own `read_seq`, then read the
/// innermost value, so what is measured is the reader's depth accounting and
/// not the test's.
fn read_nested<T>(bytes: &[u8], depth: usize) -> Result<T, ErrorCode>
where
    T: for<'de> beve::Read<'de> + Default,
{
    fn go<T>(r: &mut beve::Reader<'_>, left: usize, out: &mut T) -> Result<(), ErrorCode>
    where
        T: for<'de> beve::Read<'de>,
    {
        if left == 0 {
            return beve::Read::read(out, r);
        }
        r.read_seq(|r, _| go(r, left - 1, out)).map(|_| ())
    }
    let mut out = T::default();
    go(&mut beve::Reader::new(bytes), depth, &mut out)?;
    Ok(out)
}

#[test]
fn a_matrix_costs_the_one_level_the_skipping_walk_charges_it() {
    // A matrix holds two values, so it charges a level and each of them charges
    // its own: a matrix at N wrappers reaches N + 2. Reading and skipping have
    // to agree on that, and the way to see it is at the boundary, where one
    // level either way changes the answer.
    //
    // The data is stored narrower than the target on purpose. A `Vec` that
    // takes the bulk copy never calls `read_seq` and so charges nothing, which
    // would hide the level the arrays inside a matrix are supposed to cost.
    let inner = to_beve(&Matrix::new(MatrixLayout::RowMajor, vec![2], vec![1u8, 2]).unwrap());
    for wrappers in [
        MAX_DEPTH as usize - 3,
        MAX_DEPTH as usize - 2,
        MAX_DEPTH as usize - 1,
    ] {
        let doc = wrap(wrappers, &inner);
        let valid = validate_beve(&doc).is_ok();
        assert_eq!(
            valid,
            wrappers + 2 <= MAX_DEPTH as usize,
            "{wrappers} wrappers"
        );
        assert_eq!(
            valid,
            read_nested::<Matrix<i64>>(&doc, wrappers).is_ok(),
            "{wrappers} wrappers"
        );
    }
}

#[test]
fn a_struct_of_complex_fields_gets_the_run_form_too() {
    // Declaring an element type is what says a struct's fields are all one
    // thing, and the array it then writes is whatever that thing has.
    #[derive(Default, Debug, PartialEq)]
    struct Pair {
        a: Complex<f64>,
        b: Complex<f64>,
    }
    structio::array!(Pair [Complex<f64>; a, b]);

    let p = Pair {
        a: Complex::new(1.0, 2.0),
        b: Complex::new(3.0, 4.0),
    };
    assert_eq!(to_beve(&p), to_beve(&vec![p.a, p.b]));
    assert_eq!(from_beve::<Pair>(&to_beve(&p)).unwrap(), p);
    assert_eq!(to_string(&p), "[[1,2],[3,4]]");
}

// ---------------------------------------------------------------------------
// Matrix
// ---------------------------------------------------------------------------

#[test]
fn a_matrix_is_a_layout_byte_then_its_extents_then_its_data() {
    let m = Matrix::new(MatrixLayout::RowMajor, vec![2, 3], (0..6u8).collect()).unwrap();
    let mut want = vec![0x16, header::LAYOUT_RIGHT];
    // Extents: a typed array of one-byte unsigned integers.
    want.push(header::array_of(header::CAT_UNSIGNED, 0));
    want.extend(size(2));
    want.extend_from_slice(&[2, 3]);
    // Data: whatever array the element type has.
    want.extend(to_beve(&(0..6u8).collect::<Vec<_>>()));

    assert_eq!(to_beve(&m), want);
    assert_eq!(from_beve::<Matrix<u8>>(&want).unwrap(), m);
    assert!(validate_beve(&want).is_ok());
}

#[test]
fn both_layouts_survive_both_formats() {
    for layout in [MatrixLayout::RowMajor, MatrixLayout::ColumnMajor] {
        let m = Matrix::new(layout, vec![3, 1], vec![1.5f64, 2.5, 3.5]).unwrap();
        assert_eq!(from_beve::<Matrix<f64>>(&to_beve(&m)).unwrap(), m);
        assert_eq!(from_str::<Matrix<f64>>(&to_string(&m)).unwrap(), m);
        assert_eq!(beve_to_json(&to_beve(&m)).unwrap(), to_string(&m));
        assert!(to_string(&m).contains(layout.as_str()));

        // The names the two vocabularies give the same order are the same
        // layout, and nothing else is.
        for name in ["layout_right", "row_major", "right"] {
            assert_eq!(name.parse(), Ok(MatrixLayout::RowMajor));
        }
        for name in ["layout_left", "column_major", "left"] {
            assert_eq!(name.parse(), Ok(MatrixLayout::ColumnMajor));
        }
        assert_eq!(
            "diagonal".parse::<MatrixLayout>(),
            Err(ErrorCode::InvalidMatrixLayout)
        );
    }
}

#[test]
fn extents_are_stored_at_the_narrowest_width_that_holds_them() {
    // Dimensions are small numbers standing in front of a payload that is not,
    // so eight bytes each would be most of a small matrix. Every reader widens,
    // so nothing but the bytes is at stake.
    for (largest, code) in [(200usize, 0u8), (300, 1), (70_000, 2), (5_000_000_000, 3)] {
        // A shape whose product is zero, so the dimension being measured can be
        // as large as it likes without the test having to hold that many
        // elements.
        let m = Matrix::new(MatrixLayout::RowMajor, vec![0, largest], Vec::<u8>::new()).unwrap();
        let bytes = to_beve(&m);
        assert_eq!(
            bytes[2],
            header::array_of(header::CAT_UNSIGNED, code),
            "extents up to {largest}"
        );
        assert_eq!(from_beve::<Matrix<u8>>(&bytes).unwrap(), m);
    }
}

#[test]
fn a_matrix_reads_from_the_object_form_as_well() {
    // What a producer without the extension writes, and what the JSON side
    // always writes. Both are read back into the same type, so a document that
    // has been through a transcode still loads.
    let m = Matrix::new(MatrixLayout::ColumnMajor, vec![2, 2], vec![1u32, 2, 3, 4]).unwrap();
    let form = object(&[
        ("layout", to_beve("column_major")),
        ("extents", to_beve(&vec![2u8, 2])),
        ("value", to_beve(&vec![1u32, 2, 3, 4])),
    ]);
    assert_eq!(from_beve::<Matrix<u32>>(&form).unwrap(), m);

    let json = r#"{"layout":"layout_left","extents":[2,2],"value":[1,2,3,4]}"#;
    assert_eq!(from_str::<Matrix<u32>>(json).unwrap(), m);
}

/// Three keys is still a schema, so a fourth is an unknown key and the read
/// policy decides. The reader is hand written rather than generated, so this
/// is the one place in the crate that had to opt in by hand.
#[test]
fn a_matrix_member_the_shape_does_not_name_follows_the_policy() {
    let m = Matrix::new(MatrixLayout::ColumnMajor, vec![2, 2], vec![1u32, 2, 3, 4]).unwrap();
    let form = object(&[
        ("layout", to_beve("column_major")),
        ("extents", to_beve(&vec![2u8, 2])),
        ("value", to_beve(&vec![1u32, 2, 3, 4])),
        ("units", to_beve("volts")),
    ]);
    assert_eq!(
        from_beve::<Matrix<u32>>(&form).unwrap_err().code,
        ErrorCode::UnknownKey
    );
    assert_eq!(
        from_beve_with::<SkipUnknown, Matrix<u32>>(&form).unwrap(),
        m
    );

    let json = r#"{"units":"volts","layout":"layout_left","extents":[2,2],"value":[1,2,3,4]}"#;
    assert_eq!(
        from_str::<Matrix<u32>>(json).unwrap_err().code,
        ErrorCode::UnknownKey
    );
    assert_eq!(
        structio::from_str_with::<SkipUnknown, Matrix<u32>>(json).unwrap(),
        m
    );
}

#[test]
fn a_matrix_of_complex_numbers_is_two_extensions_and_no_special_case() {
    // The data is whatever array its element type has, so this needed no code
    // of its own on either side.
    let values = vec![
        Complex::new(1.0f64, 2.0),
        Complex::new(3.0, 4.0),
        Complex::new(5.0, 6.0),
        Complex::new(7.0, 8.0),
    ];
    let m = Matrix::new(MatrixLayout::RowMajor, vec![2, 2], values.clone()).unwrap();
    let bytes = to_beve(&m);

    // The data half is the same run a bare `Vec` would have written.
    assert!(bytes.ends_with(&to_beve(&values)));
    assert_eq!(from_beve::<Matrix<Complex<f64>>>(&bytes).unwrap(), m);
    assert!(validate_beve(&bytes).is_ok());
    assert_eq!(beve_to_json(&bytes).unwrap(), to_string(&m));
    assert_eq!(from_str::<Matrix<Complex<f64>>>(&to_string(&m)).unwrap(), m);
}

#[test]
fn a_shape_that_does_not_describe_its_data_cannot_be_built() {
    assert_eq!(
        Matrix::new(MatrixLayout::RowMajor, vec![2, 3], vec![1.0f64]).unwrap_err(),
        ErrorCode::InvalidMatrixShape
    );
    // A dimension of zero describes nothing, and is a legal empty matrix.
    assert!(Matrix::new(MatrixLayout::RowMajor, vec![0, 3], Vec::<f64>::new()).is_ok());
    // So is the default, whose extents say nothing rather than say one.
    assert_eq!(Matrix::<f64>::default().len(), 0);
    assert!(Matrix::<f64>::default().is_empty());
    assert_eq!(Matrix::<f64>::default().rank(), 0);
    // Extents that multiply out past the address space describe no data there
    // could ever be.
    assert_eq!(
        Matrix::new(MatrixLayout::RowMajor, vec![usize::MAX, 2], vec![1.0f64]).unwrap_err(),
        ErrorCode::InvalidMatrixShape
    );
}

#[test]
fn a_matrix_that_fails_to_read_is_left_empty_rather_than_half_filled() {
    // Everywhere else in the crate a failed read leaves a partially written
    // value, which is fine for a struct whose fields stand alone. Here it would
    // leave a matrix stating a shape it does not hold, which is the one thing
    // the type promises never to do.
    // Deliberately the layout that is *not* the default, so that resetting it
    // and keeping it are telling apart at all.
    let mut bad = vec![0x16, header::LAYOUT_LEFT];
    bad.push(header::array_of(header::CAT_UNSIGNED, 0));
    bad.extend(size(2));
    bad.extend_from_slice(&[2, 3]);
    bad.extend(to_beve(&vec![1.0f64, 2.0])); // two elements where six were promised

    // The document is well formed; only its meaning is wrong.
    assert!(validate_beve(&bad).is_ok());

    assert_ne!(MatrixLayout::ColumnMajor, MatrixLayout::default());
    let mut m = Matrix::new(MatrixLayout::RowMajor, vec![1], vec![9.0f64]).unwrap();
    assert_eq!(
        structio::read_beve_into(&mut m, &bad).unwrap_err().code,
        ErrorCode::InvalidMatrixShape
    );
    assert_eq!(m.extents(), &[] as &[usize]);
    assert_eq!(m.data(), &[] as &[f64]);
    // The layout goes back with them. Two thirds of a document is not a matrix
    // that was read, and a layout is the field whose being wrong shows up
    // nowhere at all.
    assert_eq!(m.layout(), MatrixLayout::default());

    // And the same for a read that fails partway rather than at the shape.
    let mut m = Matrix::new(MatrixLayout::ColumnMajor, vec![1], vec![9.0f64]).unwrap();
    assert!(structio::read_beve_into(&mut m, &bad[..bad.len() - 3]).is_err());
    assert!(m.is_empty() && m.rank() == 0 && m.layout() == MatrixLayout::default());
}

#[test]
fn a_layout_byte_that_is_not_defined_is_refused() {
    // Reading it wrongly transposes the data silently, so it is never guessed.
    let mut bytes = to_beve(&Matrix::new(MatrixLayout::RowMajor, vec![2], vec![1u8, 2]).unwrap());
    bytes[1] = 2;
    assert_eq!(
        from_beve::<Matrix<u8>>(&bytes).unwrap_err().code,
        ErrorCode::InvalidMatrixLayout
    );
    // The byte is one byte wherever it points, so it threatens no extent and
    // `validate` has no reason to look at it.
    assert!(validate_beve(&bytes).is_ok());
}

#[test]
fn anything_that_is_not_a_matrix_is_refused() {
    for bytes in [to_beve(&1u8), to_beve("m"), to_beve(&vec![1u8, 2])] {
        assert_eq!(
            from_beve::<Matrix<u8>>(&bytes).unwrap_err().code,
            ErrorCode::ExpectedMatrix
        );
    }
}

#[test]
fn a_matrix_member_that_is_not_wanted_is_stepped_over() {
    let m = Matrix::new(
        MatrixLayout::ColumnMajor,
        vec![2, 2],
        vec![1.0f64, 2.0, 3.0, 4.0],
    )
    .unwrap();
    let doc = object(&[
        ("a", to_beve(&1u32)),
        ("z", to_beve(&m)),
        ("b", to_beve(&2u32)),
    ]);
    assert_eq!(
        from_beve_with::<SkipUnknown, Two>(&doc).unwrap(),
        Two { a: 1, b: 2 }
    );

    // And reached by pointer, which is what stepping over the rest is for.
    assert_eq!(
        structio::from_beve_at::<Matrix<f64>>(&doc, "/z").unwrap(),
        m
    );
}

#[test]
fn a_borrowed_matrix_writes_what_an_owned_one_would() {
    let extents = [2usize, 2];
    let data = [1.0f64, 2.0, 3.0, 4.0];
    let view = MatrixRef::new(MatrixLayout::ColumnMajor, &extents, &data).unwrap();
    let owned = view.to_matrix();

    assert_eq!(to_beve(&view), to_beve(&owned));
    assert_eq!(to_string(&view), to_string(&owned));
    assert_eq!(from_beve::<Matrix<f64>>(&to_beve(&view)).unwrap(), owned);

    assert_eq!(
        MatrixRef::new(MatrixLayout::RowMajor, &extents, &data[..3]).unwrap_err(),
        ErrorCode::InvalidMatrixShape
    );
}

#[test]
fn the_pieces_come_back_apart() {
    let mut m = Matrix::new(
        MatrixLayout::RowMajor,
        vec![2, 2],
        vec![1.0f64, 2.0, 3.0, 4.0],
    )
    .unwrap();
    m.data_mut()[0] = 9.0;
    m.set_layout(MatrixLayout::ColumnMajor);
    let (layout, extents, data) = m.into_parts();
    assert_eq!(layout, MatrixLayout::ColumnMajor);
    assert_eq!(extents, vec![2, 2]);
    assert_eq!(data, vec![9.0, 2.0, 3.0, 4.0]);
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

#[test]
fn a_run_of_complex_numbers_streams_element_by_element() {
    // A complex element carries no header, so array mode can only hand one out
    // if the splitter supplies the header the run implied -- which is exactly
    // what the synthetic element header is, one byte carrying the class and the
    // width. So this needs nothing a typed array does not.
    let run: Vec<Complex<f64>> = (0..64)
        .map(|i| Complex::new(i as f64, -(i as f64)))
        .collect();
    let bytes = to_beve(&run);

    let mut docs = beve::Documents::array(&bytes[..]).read_size(16);
    let pulled: Vec<Complex<f64>> = docs.iter::<Complex<f64>>().map(Result::unwrap).collect();
    assert_eq!(pulled, run);

    // Stored narrower than the target, which is the element-by-element path.
    let narrow = to_beve(&vec![Complex::new(1.0f32, 2.0), Complex::new(3.0, 4.0)]);
    let mut docs = beve::Documents::array(&narrow[..]);
    let pulled: Vec<Complex<f64>> = docs.iter::<Complex<f64>>().map(Result::unwrap).collect();
    assert_eq!(pulled, vec![Complex::new(1.0, 2.0), Complex::new(3.0, 4.0)]);

    // A lone complex number is one value, not a sequence of one.
    let lone = to_beve(&Complex::new(1.0f64, 2.0));
    let mut docs = beve::Documents::array(&lone[..]);
    assert!(
        docs.next_value::<Complex<f64>>()
            .is_some_and(|r| r.is_err())
    );
}
