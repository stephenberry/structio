//! Positional structs: the `array!` family.
//!
//! The property worth the most here is the last one checked: an `array!`
//! struct and the tuple of the same field types produce identical bytes, in
//! both formats. That is what "a tuple is this encoding without the names"
//! has to mean, and it is only true if both go through the same drivers.

use structio::{ErrorCode, from_beve, from_str, to_beve, to_string};

#[derive(Default, Debug, PartialEq)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}
structio::array!(Vec3 [x, y, z]);

#[derive(Default, Debug, PartialEq)]
struct Mixed {
    name: String,
    count: u32,
    active: bool,
}
structio::array!(Mixed [name, count, active]);

fn vec3() -> Vec3 {
    Vec3 {
        x: 1.5,
        y: -2.0,
        z: 3.25,
    }
}

#[test]
fn json_roundtrip() {
    let json = "[1.5,-2,3.25]";
    assert_eq!(to_string(&vec3()), json);
    assert_eq!(from_str::<Vec3>(json).unwrap(), vec3());
}

#[test]
fn beve_roundtrip() {
    let bytes = to_beve(&vec3());
    assert_eq!(from_beve::<Vec3>(&bytes).unwrap(), vec3());
}

#[test]
fn elements_may_have_different_types() {
    let m = Mixed {
        name: "a\"b".into(),
        count: 7,
        active: true,
    };
    let json = r#"["a\"b",7,true]"#;
    assert_eq!(to_string(&m), json);
    assert_eq!(from_str::<Mixed>(json).unwrap(), m);
    assert_eq!(from_beve::<Mixed>(&to_beve(&m)).unwrap(), m);
}

#[test]
fn whitespace_everywhere() {
    let json = "  [ 1.5 , -2 ,\n\t3.25 ]  ";
    assert_eq!(from_str::<Vec3>(json).unwrap(), vec3());
}

/// The trade an array declaration makes: there is no such thing as a missing
/// element or an extra one, only a document of the wrong length.
#[test]
fn length_must_match_exactly() {
    for json in ["[]", "[1]", "[1,2]", "[1,2,3,4]"] {
        let err = from_str::<Vec3>(json).unwrap_err();
        assert_eq!(err.code, ErrorCode::ArrayLengthMismatch, "on {json}");
    }
    assert!(from_str::<Vec3>("[1,2,3]").is_ok());
}

#[test]
fn length_must_match_exactly_in_beve() {
    let short = to_beve(&(1.0f64, 2.0f64));
    let long = to_beve(&(1.0f64, 2.0f64, 3.0f64, 4.0f64));
    for bytes in [short, long] {
        let err = from_beve::<Vec3>(&bytes).unwrap_err();
        assert_eq!(err.code, ErrorCode::ArrayLengthMismatch);
    }
}

#[test]
fn an_object_is_not_an_array() {
    let err = from_str::<Vec3>(r#"{"x":1,"y":2,"z":3}"#).unwrap_err();
    assert_eq!(err.code, ErrorCode::ExpectedBracket);

    let err = from_beve::<Vec3>(&to_beve(&Shape::default())).unwrap_err();
    assert_eq!(err.code, ErrorCode::ExpectedArray);
}

#[test]
fn malformed_input_errors_rather_than_panics() {
    let json = "[1.5,-2,3.25]";
    for cut in 0..json.len() {
        assert!(from_str::<Vec3>(&json[..cut]).is_err());
    }
    for bad in ["[1,,2]", "[1 2 3]", "[1,2,3", "1,2,3]", "[[1,2,3]]"] {
        assert!(from_str::<Vec3>(bad).is_err(), "on {bad}");
    }
}

// ---------------------------------------------------------------------------
// Composition
// ---------------------------------------------------------------------------

#[derive(Default, Debug, PartialEq, Clone, Copy)]
struct Point {
    x: i32,
    y: i32,
}
structio::array!(Point [x, y]);

#[derive(Default, Debug, PartialEq)]
struct Shape {
    label: String,
    origin: Point,
    path: Vec<Point>,
}
structio::object!(Shape {
    label,
    origin,
    path
});

#[test]
fn arrays_nest_inside_objects_and_sequences() {
    let s = Shape {
        label: "tri".into(),
        origin: Point { x: 1, y: 2 },
        path: vec![Point { x: 0, y: 0 }, Point { x: 3, y: 4 }],
    };
    let json = r#"{"label":"tri","origin":[1,2],"path":[[0,0],[3,4]]}"#;
    assert_eq!(to_string(&s), json);
    assert_eq!(from_str::<Shape>(json).unwrap(), s);
    assert_eq!(from_beve::<Shape>(&to_beve(&s)).unwrap(), s);
}

#[derive(Default, Debug, PartialEq)]
struct Nested {
    head: Point,
    tail: Point,
}
structio::array!(Nested [head, tail]);

#[test]
fn arrays_nest_inside_arrays() {
    let n = Nested {
        head: Point { x: 1, y: 2 },
        tail: Point { x: 3, y: 4 },
    };
    let json = "[[1,2],[3,4]]";
    assert_eq!(to_string(&n), json);
    assert_eq!(from_str::<Nested>(json).unwrap(), n);
    assert_eq!(from_beve::<Nested>(&to_beve(&n)).unwrap(), n);
}

// ---------------------------------------------------------------------------
// Declaration forms
// ---------------------------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Trailing {
    a: u8,
    b: u8,
}
structio::array!(Trailing [a, b,]);

#[derive(Default, Debug, PartialEq)]
struct Nothing {}
structio::array!(Nothing []);

#[test]
fn a_declaration_may_be_empty_or_trailing_comma() {
    assert_eq!(to_string(&Trailing { a: 1, b: 2 }), "[1,2]");
    assert_eq!(to_string(&Nothing {}), "[]");
    assert_eq!(from_str::<Nothing>("[]").unwrap(), Nothing {});
    assert_eq!(
        from_str::<Nothing>("[1]").unwrap_err().code,
        ErrorCode::ArrayLengthMismatch
    );
    assert_eq!(
        from_beve::<Nothing>(&to_beve(&Nothing {})).unwrap(),
        Nothing {}
    );
}

#[derive(Default, Debug, PartialEq)]
struct Borrowed<'a> {
    name: &'a str,
    n: u32,
}
structio::array!(['de] Borrowed<'de> [name, n]);

/// The same shape under the struct's own lifetime name.
#[derive(Default, Debug, PartialEq)]
struct Named<'a> {
    name: &'a str,
    n: u32,
}
structio::array!(['a] Named<'a> [name, n]);

#[test]
fn a_borrowed_array_keeps_its_own_lifetime_name() {
    let json = String::from(r#"["row",3]"#);
    let row: Named = structio::from_str(&json).unwrap();
    assert_eq!(row, Named { name: "row", n: 3 });
    assert!(std::ptr::eq(row.name.as_ptr(), json[2..].as_ptr()));
    assert_eq!(structio::to_string(&row), json);
}

#[test]
fn borrowed_elements() {
    let json = r#"["hi",3]"#;
    let b: Borrowed = from_str(json).unwrap();
    assert_eq!(b, Borrowed { name: "hi", n: 3 });
    assert_eq!(to_string(&b), json);
    // The borrow is out of the document, not a copy.
    assert_eq!(b.name.as_ptr(), json[2..].as_ptr());
}

#[derive(Default, Debug, PartialEq)]
struct Pair<T> {
    first: T,
    second: T,
}
structio::array!([T: structio::ReadWrite + Default] Pair<T> [first, second]);

#[test]
fn generic_elements() {
    let p = Pair {
        first: "a".to_string(),
        second: "b".to_string(),
    };
    assert_eq!(to_string(&p), r#"["a","b"]"#);
    assert_eq!(from_str::<Pair<String>>(r#"["a","b"]"#).unwrap(), p);
    assert_eq!(from_beve::<Pair<String>>(&to_beve(&p)).unwrap(), p);
}

#[derive(Default, Debug, PartialEq)]
struct JsonOnly {
    a: u8,
}
structio::json_array!(JsonOnly[a]);

#[derive(Default, Debug, PartialEq)]
struct BeveOnly<'a> {
    id: u32,
    payload: &'a [u8],
}
structio::beve_array!(['de] BeveOnly<'de> [id, payload]);

#[test]
fn one_format_only() {
    assert_eq!(to_string(&JsonOnly { a: 7 }), "[7]");

    let f = BeveOnly {
        id: 9,
        payload: &[1, 2, 3],
    };
    let bytes = to_beve(&f);
    assert_eq!(from_beve::<BeveOnly>(&bytes).unwrap(), f);
}

// ---------------------------------------------------------------------------
// Equivalence with tuples
// ---------------------------------------------------------------------------

#[test]
fn a_named_struct_and_a_tuple_encode_identically() {
    let tuple = (1.5f64, -2.0f64, 3.25f64);
    assert_eq!(to_string(&vec3()), to_string(&tuple));
    assert_eq!(to_beve(&vec3()), to_beve(&tuple));

    let m = Mixed {
        name: "a".into(),
        count: 7,
        active: true,
    };
    let tuple = ("a".to_string(), 7u32, true);
    assert_eq!(to_string(&m), to_string(&tuple));
    assert_eq!(to_beve(&m), to_beve(&tuple));
}

/// BEVE stores a run of numbers as one block with one header, and the array
/// driver hands out its elements one at a time regardless. So a struct of
/// three `f64`s reads back from the typed array another implementation would
/// have written for a `[f64; 3]`, which is the interoperable case.
#[test]
fn beve_reads_a_typed_array_into_a_positional_struct() {
    let typed = to_beve(&[1.5f64, -2.0, 3.25]);
    assert_eq!(from_beve::<Vec3>(&typed).unwrap(), vec3());

    let wrong_length = to_beve(&[1.5f64, -2.0]);
    assert_eq!(
        from_beve::<Vec3>(&wrong_length).unwrap_err().code,
        ErrorCode::ArrayLengthMismatch
    );
}

// ---------------------------------------------------------------------------
// Reuse
// ---------------------------------------------------------------------------

#[test]
fn reading_into_an_existing_value_keeps_its_allocation() {
    let mut m = Mixed {
        name: String::with_capacity(64),
        ..Mixed::default()
    };
    let before = m.name.as_ptr();
    structio::read_into(&mut m, r#"["short",1,false]"#).unwrap();
    assert_eq!(m.name, "short");
    assert_eq!(m.name.as_ptr(), before);
}

// ---------------------------------------------------------------------------
// Homogeneous structs, as BEVE typed arrays
// ---------------------------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}
structio::array!(Rgb [u8; r, g, b]);

#[derive(Default, Debug, PartialEq)]
struct Typed3 {
    x: f64,
    y: f64,
    z: f64,
}
structio::array!(Typed3 [f64; x, y, z]);

#[derive(Default, Debug, PartialEq)]
struct Flags {
    a: bool,
    b: bool,
    c: bool,
}
structio::array!(Flags [bool; a, b, c]);

/// The point of naming an element type: the struct is stored the way a run of
/// that type is stored, so its bytes are a slice's bytes.
#[test]
fn a_typed_struct_encodes_as_the_slice_would() {
    assert_eq!(
        to_beve(&Rgb {
            r: 10,
            g: 20,
            b: 30
        }),
        to_beve(&[10u8, 20, 30])
    );
    assert_eq!(to_beve(&vec3typed()), to_beve(&[1.5f64, -2.0, 3.25]));
    assert_eq!(
        to_beve(&Flags {
            a: true,
            b: false,
            c: true
        }),
        to_beve(&[true, false, true])
    );
}

fn vec3typed() -> Typed3 {
    Typed3 {
        x: 1.5,
        y: -2.0,
        z: 3.25,
    }
}

#[test]
fn a_typed_struct_round_trips() {
    for value in [
        Typed3::default(),
        vec3typed(),
        Typed3 {
            x: f64::MIN,
            y: 0.0,
            z: f64::MAX,
        },
    ] {
        assert_eq!(from_beve::<Typed3>(&to_beve(&value)).unwrap(), value);
    }

    let c = Rgb {
        r: 0,
        g: 128,
        b: 255,
    };
    assert_eq!(from_beve::<Rgb>(&to_beve(&c)).unwrap(), c);

    let f = Flags {
        a: false,
        b: true,
        c: true,
    };
    assert_eq!(from_beve::<Flags>(&to_beve(&f)).unwrap(), f);
}

/// Booleans pack one per bit in a typed array, which is the case a per-element
/// payload writer would have got wrong.
#[test]
fn booleans_pack_to_bits() {
    let bytes = to_beve(&Flags {
        a: true,
        b: false,
        c: true,
    });
    // Header, count, and one byte holding all three bits.
    assert_eq!(bytes.len(), 3);
    assert_eq!(bytes[2], 0b101);
}

#[test]
fn a_typed_struct_is_smaller_than_a_generic_one() {
    #[derive(Default, Debug, PartialEq)]
    struct Generic3 {
        x: f64,
        y: f64,
        z: f64,
    }
    structio::array!(Generic3 [x, y, z]);

    let generic = to_beve(&Generic3 {
        x: 1.5,
        y: -2.0,
        z: 3.25,
    });
    assert!(to_beve(&vec3typed()).len() < generic.len());
    // And the generic form still reads into the typed struct: naming an
    // element type changes what is written, not what is accepted.
    assert_eq!(from_beve::<Typed3>(&generic).unwrap(), vec3typed());
}

#[test]
fn json_is_unaffected_by_the_element_type() {
    assert_eq!(to_string(&vec3typed()), "[1.5,-2,3.25]");
    assert_eq!(from_str::<Typed3>("[1.5,-2,3.25]").unwrap(), vec3typed());
    assert_eq!(to_string(&Rgb { r: 1, g: 2, b: 3 }), "[1,2,3]");
    assert_eq!(
        from_str::<Typed3>("[1,2]").unwrap_err().code,
        ErrorCode::ArrayLengthMismatch
    );
}

/// An element type with no typed array of its own is not an error, it just
/// leaves the encoding generic.
#[derive(Default, Debug, PartialEq)]
struct Segment {
    from: Point,
    to: Point,
}
structio::array!(Segment [Point; from, to]);

#[test]
fn an_element_type_without_a_typed_array_stays_generic() {
    let s = Segment {
        from: Point { x: 1, y: 2 },
        to: Point { x: 3, y: 4 },
    };
    assert_eq!(to_string(&s), "[[1,2],[3,4]]");
    assert_eq!(from_beve::<Segment>(&to_beve(&s)).unwrap(), s);
    // Byte for byte what the same struct declared without an element type
    // would have written.
    assert_eq!(
        to_beve(&s),
        to_beve(&Nested {
            head: Point { x: 1, y: 2 },
            tail: Point { x: 3, y: 4 },
        })
    );
}

#[derive(Default, Debug, PartialEq)]
struct TypedPair<T> {
    first: T,
    second: T,
}
structio::array!([T: structio::ReadWrite + Default + Copy] TypedPair<T> [T; first, second]);

#[test]
fn a_generic_element_type_is_typed_too() {
    let p = TypedPair {
        first: 1.5f32,
        second: -2.5f32,
    };
    assert_eq!(to_beve(&p), to_beve(&[1.5f32, -2.5]));
    assert_eq!(from_beve::<TypedPair<f32>>(&to_beve(&p)).unwrap(), p);

    let q = TypedPair {
        first: 7u16,
        second: 9u16,
    };
    assert_eq!(to_beve(&q), to_beve(&[7u16, 9]));
}

#[derive(Default, Debug, PartialEq)]
struct JsonTyped {
    a: u32,
    b: u32,
}
structio::json_array!(JsonTyped [u32; a, b]);

#[test]
fn the_element_type_is_accepted_by_the_single_format_macros() {
    assert_eq!(to_string(&JsonTyped { a: 1, b: 2 }), "[1,2]");
    assert_eq!(
        from_str::<JsonTyped>("[1,2]").unwrap(),
        JsonTyped { a: 1, b: 2 }
    );
}
