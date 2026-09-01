//! Enums, in both formats.
//!
//! Two wire forms and no third: a variant carrying nothing is its name, and a
//! variant carrying a value is that name used as the single key of an object.
//! What is checked here is that both formats agree on which is which, that
//! reading takes either form for either kind of variant, and that a tag naming
//! nothing is refused rather than guessed at.

use structio::{
    ErrorCode, Pretty, RequireKeys, SkipNull, SkipUnknown, from_beve, from_str, to_beve, to_string,
};

#[derive(Default, PartialEq, Debug, Clone)]
enum Level {
    #[default]
    Info,
    Warning,
    Error,
}
structio::unit_enum!(Level {
    Info,
    Warning,
    Error
});

#[derive(Default, PartialEq, Debug, Clone)]
struct Circle {
    radius: f64,
}
structio::object!(Circle { radius });

#[derive(Default, PartialEq, Debug, Clone)]
enum Shape {
    #[default]
    Empty,
    Circle(Circle),
    Sides(u32),
    Label(String),
    Corners((f64, f64)),
}
structio::tagged_enum!(Shape {
    Empty,
    Circle(_),
    Sides(_),
    Label(_),
    Corners(_),
});

/// Every declaration form in one type: a renamed unit variant, a renamed
/// payload variant, and a payload that is itself an enum.
#[derive(Default, PartialEq, Debug, Clone)]
enum Event {
    #[default]
    Connected,
    Log(Level),
    Shape(Shape),
}
structio::tagged_enum!(Event {
    "connected" => Connected,
    "log" => Log(_),
    "shape" => Shape(_),
});

/// A generic unit enum, which takes `unit_enum!`'s bracketed-generics rule
/// rather than its bare one. The parameter is a const, because Rust will not
/// take a type or lifetime parameter that no variant uses and a variant that
/// used one would carry a value.
#[derive(Default, PartialEq, Debug, Clone)]
enum Slot<const N: usize> {
    #[default]
    Free,
    Taken,
}
structio::unit_enum!([const N: usize] Slot<N> { Free, Taken });

/// An enum whose payload borrows out of the input, which is what `'de` in the
/// generics list is for.
#[derive(Default, PartialEq, Debug)]
enum Ref<'a> {
    #[default]
    Nothing,
    Text(&'a str),
}
structio::tagged_enum!(['de] Ref<'de> { Nothing, Text(_) });

/// A generic enum, declared the way a generic struct is.
#[derive(Default, PartialEq, Debug, Clone)]
enum Message<T> {
    #[default]
    Ping,
    Data(T),
}
structio::tagged_enum!([T: structio::ReadWrite + Default] Message<T> { Ping, Data(_) });

/// Both formats, in both directions, for one value.
#[track_caller]
fn roundtrip<T>(value: &T, json: &str)
where
    T: structio::ReadWrite + PartialEq + core::fmt::Debug + Default,
{
    assert_eq!(to_string(value), json, "json bytes");
    assert_eq!(&from_str::<T>(json).unwrap(), value, "json read back");
    assert_eq!(
        &from_beve::<T>(&to_beve(value)).unwrap(),
        value,
        "beve round trip"
    );
    // The two formats hold the same value, which is what a shared schema means.
    assert_eq!(
        to_string(&from_beve::<T>(&to_beve(value)).unwrap()),
        json,
        "beve to json"
    );
}

// ---------------------------------------------------------------------------
// The two wire forms
// ---------------------------------------------------------------------------

#[test]
fn a_variant_carrying_nothing_is_its_name() {
    roundtrip(&Level::Info, "\"Info\"");
    roundtrip(&Level::Warning, "\"Warning\"");
    roundtrip(&Level::Error, "\"Error\"");
    roundtrip(&Shape::Empty, "\"Empty\"");
}

#[test]
fn a_variant_carrying_a_value_is_an_object_of_one_member() {
    roundtrip(
        &Shape::Circle(Circle { radius: 2.5 }),
        r#"{"Circle":{"radius":2.5}}"#,
    );
    roundtrip(&Shape::Sides(3), r#"{"Sides":3}"#);
    roundtrip(&Shape::Label("a".into()), r#"{"Label":"a"}"#);
    roundtrip(&Shape::Corners((1.0, 2.0)), r#"{"Corners":[1,2]}"#);
}

#[test]
fn a_name_may_differ_from_the_rust_one() {
    roundtrip(&Event::Connected, "\"connected\"");
    roundtrip(&Event::Log(Level::Error), r#"{"log":"Error"}"#);
    roundtrip(&Event::Shape(Shape::Sides(4)), r#"{"shape":{"Sides":4}}"#);
}

#[test]
fn a_generic_enum_takes_its_parameter_like_a_struct_does() {
    roundtrip(&Message::<u32>::Ping, "\"Ping\"");
    roundtrip(&Message::Data(vec![1u8, 2]), r#"{"Data":[1,2]}"#);
}

#[test]
fn generics_and_borrowing_take_the_forms_a_struct_takes() {
    // A generic unit enum: the parameter never reaches the wire, since no
    // variant carries anything, but the declaration still has to name it.
    roundtrip(&Slot::<4>::Free, "\"Free\"");
    assert_eq!(to_string(&Slot::<9>::Taken), "\"Taken\"");

    // A borrowing payload points into the document rather than copying out of
    // it, which is the whole reason `'de` is written out.
    let text = r#"{"Text":"borrowed"}"#;
    let value = from_str::<Ref>(text).unwrap();
    assert_eq!(value, Ref::Text("borrowed"));
    match value {
        Ref::Text(s) => {
            let at = text.find("borrowed").unwrap();
            assert!(std::ptr::eq(s.as_ptr(), text[at..].as_ptr()));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(from_str::<Ref>("\"Nothing\"").unwrap(), Ref::Nothing);
}

#[test]
fn an_enum_nests_wherever_a_value_goes() {
    #[derive(Default, PartialEq, Debug)]
    struct Doc {
        level: Level,
        shapes: Vec<Shape>,
    }
    structio::object!(Doc { level, shapes });

    roundtrip(
        &Doc {
            level: Level::Warning,
            shapes: vec![Shape::Empty, Shape::Sides(6)],
        },
        r#"{"level":"Warning","shapes":["Empty",{"Sides":6}]}"#,
    );
}

// ---------------------------------------------------------------------------
// What reading accepts
// ---------------------------------------------------------------------------

#[test]
fn the_object_form_is_accepted_for_a_variant_carrying_nothing() {
    // Written as `"Empty"`, but a producer that always writes the object form
    // is still understood. Nothing else stands in for the absent value.
    assert_eq!(
        from_str::<Shape>(r#"{"Empty":null}"#).unwrap(),
        Shape::Empty
    );
    assert_eq!(from_str::<Level>(r#"{"Info":null}"#).unwrap(), Level::Info);
    assert_eq!(
        from_str::<Shape>(r#"{"Empty":0}"#).unwrap_err().code,
        ErrorCode::ExpectedNull
    );

    // BEVE says the same thing in its own bytes, written out literally rather
    // than through a writer that would never produce this form.
    let mut doc = vec![
        0x03, // an object, string keys
        0x04, // holding one member
        0x14, // whose key is five bytes
    ];
    doc.extend_from_slice(b"Empty");
    doc.push(0x00); // null
    assert_eq!(from_beve::<Shape>(&doc).unwrap(), Shape::Empty);
}

#[test]
fn a_variant_carrying_a_value_has_no_bare_form() {
    // The name is recognized; what is missing is the value under it, so this
    // is not an unknown variant.
    let e = from_str::<Shape>("\"Circle\"").unwrap_err();
    assert_eq!(e.code, ErrorCode::ExpectedBrace);
    assert_eq!(
        from_beve::<Shape>(&to_beve("Circle")).unwrap_err().code,
        ErrorCode::ExpectedObject
    );
}

#[test]
fn a_name_no_variant_claims_is_refused() {
    for text in ["\"Round\"", r#"{"Round":1}"#] {
        assert_eq!(
            from_str::<Shape>(text).unwrap_err().code,
            ErrorCode::UnknownVariant,
            "{text}"
        );
    }
    assert_eq!(
        from_beve::<Shape>(&to_beve("Round")).unwrap_err().code,
        ErrorCode::UnknownVariant
    );
}

#[test]
fn a_name_that_only_collides_with_one_is_refused() {
    use structio::Variants;

    // The hash proposes a variant; the name itself decides. Asserting the
    // refusal alone would not say which of the two happened, and the case that
    // matters is the one that *reaches* the confirmation, so the proposal is
    // pinned first: these names hash into the table and are then thrown out by
    // the comparison, rather than falling out of it on the way in.
    for name in ["Emptz", "Sider"] {
        let index = Shape::MAP.lookup_sized(Shape::VARIANTS, name.as_bytes());
        assert!(
            index < Shape::VARIANTS.len(),
            "{name} was meant to collide, but the table refused it outright"
        );
        assert_eq!(
            from_str::<Shape>(&format!("\"{name}\"")).unwrap_err().code,
            ErrorCode::UnknownVariant,
            "{name}"
        );
    }

    // A prefix of a real name is not that name either.
    assert_eq!(
        from_str::<Shape>("\"Empt\"").unwrap_err().code,
        ErrorCode::UnknownVariant
    );
}

#[test]
fn a_document_that_ended_is_not_a_document_that_held_the_wrong_thing() {
    // Every other reader here tells those two apart, and the formats have to
    // agree with each other as well as with the rest of the crate.
    for text in ["", "  "] {
        assert_eq!(
            from_str::<Shape>(text).unwrap_err().code,
            ErrorCode::UnexpectedEnd,
            "{text:?}"
        );
    }
    assert_eq!(
        from_beve::<Shape>(&[]).unwrap_err().code,
        ErrorCode::UnexpectedEnd
    );

    // A name the input ended in the middle of is not a name that went
    // unrecognized: `match_key` fails identically for both, so the name is
    // walked to its closing quote to tell them apart.
    for text in ["\"Empty", "{\"Round", "{\"Sides"] {
        assert_eq!(
            from_str::<Shape>(text).unwrap_err().code,
            ErrorCode::UnexpectedEnd,
            "{text:?}"
        );
    }
}

#[test]
fn an_unknown_variant_is_refused_under_every_policy() {
    // Unlike an unknown object key: a member with nowhere to go can be stepped
    // over and the object still read, but a variant with nowhere to go leaves
    // the value itself undecided.
    let e = structio::from_str_with::<SkipUnknown, Shape>("\"Round\"").unwrap_err();
    assert_eq!(e.code, ErrorCode::UnknownVariant);
    let bytes = to_beve("Round");
    let e = structio::from_beve_with::<SkipUnknown, Shape>(&bytes).unwrap_err();
    assert_eq!(e.code, ErrorCode::UnknownVariant);
}

#[test]
fn an_object_that_holds_anything_but_one_tag_is_not_a_variant() {
    for text in [
        "{}",
        r#"{"Sides":1,"Empty":null}"#,
        "1",
        "[]",
        "null",
        "true",
    ] {
        // One code, not a set of acceptable ones: an object holding no tag and
        // one holding two are the same refusal as a value that is not an object
        // at all.
        assert_eq!(
            from_str::<Shape>(text).unwrap_err().code,
            ErrorCode::ExpectedVariant,
            "{text}"
        );
    }

    // BEVE states its member count up front, so both the empty object and the
    // two-member one are refused before any name is read.
    for members in [0u8, 2] {
        let mut doc = vec![0x03, members << 2];
        for name in ["Empty", "Sides"].iter().take(members as usize) {
            doc.push((name.len() as u8) << 2);
            doc.extend_from_slice(name.as_bytes());
            doc.push(0x00); // null
        }
        assert_eq!(
            from_beve::<Shape>(&doc).unwrap_err().code,
            ErrorCode::ExpectedVariant,
            "{members} members"
        );
    }
}

#[test]
fn an_error_points_at_the_name_that_was_not_recognized() {
    let e = from_str::<Shape>(r#"  {"Round":1}"#).unwrap_err();
    assert_eq!(e.code, ErrorCode::UnknownVariant);
    // The name, not the brace before it and not the value after it.
    assert_eq!(e.index, 4);

    let e = from_str::<Shape>("  \"Round\"").unwrap_err();
    assert_eq!(e.index, 3);
}

#[test]
fn a_run_of_unit_variants_is_a_beve_string_array() {
    // A unit enum's value is a string and can be nothing else, so a run of them
    // is stored the way a run of strings is: one header for the lot rather than
    // one per element. The bytes are asserted against `Vec<String>` rather than
    // written out, since being *the same encoding* is the whole claim.
    let levels = vec![Level::Info, Level::Error, Level::Warning];
    let names: Vec<String> = vec!["Info".into(), "Error".into(), "Warning".into()];
    assert_eq!(to_beve(&levels), to_beve(&names));
    assert_eq!(from_beve::<Vec<Level>>(&to_beve(&levels)).unwrap(), levels);
    assert_eq!(from_beve::<Vec<Level>>(&to_beve(&names)).unwrap(), levels);
    assert_eq!(
        to_beve(&Vec::<Level>::new()),
        to_beve(&Vec::<String>::new())
    );

    // A generic array of the same names is still accepted, so the two forms
    // stay interchangeable and a producer that writes either is understood.
    let generic = to_beve(&vec![Some("Info"), Some("Error"), Some("Warning")]);
    assert_eq!(from_beve::<Vec<Level>>(&generic).unwrap(), levels);

    // A tagged enum has no string array, since a variant carrying a value
    // writes an object, so its runs stay generic arrays.
    let shapes = vec![Shape::Empty, Shape::Sides(1)];
    assert_eq!(from_beve::<Vec<Shape>>(&to_beve(&shapes)).unwrap(), shapes);
}

// ---------------------------------------------------------------------------
// Policies
// ---------------------------------------------------------------------------

#[test]
fn the_tag_object_is_laid_out_like_any_other() {
    assert_eq!(
        structio::to_string_with::<Pretty, _>(&Shape::Circle(Circle { radius: 1.0 })),
        "{\n  \"Circle\": {\n    \"radius\": 1\n  }\n}"
    );
    assert_eq!(
        structio::to_string_with::<Pretty, _>(&Shape::Empty),
        "\"Empty\""
    );
    // And a prettified tag is a tag: the minifier and the reader agree.
    let pretty = structio::to_string_with::<Pretty, _>(&Shape::Sides(2));
    assert_eq!(structio::minify(&pretty).unwrap(), r#"{"Sides":2}"#);
    assert_eq!(from_str::<Shape>(&pretty).unwrap(), Shape::Sides(2));
}

#[test]
fn skip_null_does_not_reach_the_tag() {
    // Dropping the member would leave `{}`, which names no variant: a
    // different value, not a shorter spelling of this one.
    #[derive(Default, PartialEq, Debug)]
    enum Maybe {
        #[default]
        Nothing,
        Value(Option<u32>),
    }
    structio::tagged_enum!(Maybe { Nothing, Value(_) });

    assert_eq!(
        structio::to_string_with::<SkipNull, _>(&Maybe::Value(None)),
        r#"{"Value":null}"#
    );
    let bytes = structio::to_beve_with::<SkipNull, _>(&Maybe::Value(None));
    assert_eq!(from_beve::<Maybe>(&bytes).unwrap(), Maybe::Value(None));
}

#[test]
fn a_policy_that_requires_keys_reaches_the_payload_and_not_the_tag() {
    // The tag object has one member, which is the variant; it is not a struct
    // and has no keys to require. The payload underneath still has its own.
    assert_eq!(
        structio::from_str_with::<RequireKeys, Shape>(r#"{"Circle":{"radius":1}}"#).unwrap(),
        Shape::Circle(Circle { radius: 1.0 })
    );
    assert_eq!(
        structio::from_str_with::<RequireKeys, Shape>(r#"{"Circle":{}}"#)
            .unwrap_err()
            .code,
        ErrorCode::MissingKey
    );
}

#[test]
fn comments_go_where_whitespace_goes() {
    use structio::AllowComments;
    let text = r#"/* tag */ { /* name */ "Sides" /* colon */ : 7 }"#;
    assert_eq!(
        structio::from_str_with::<AllowComments, Shape>(text).unwrap(),
        Shape::Sides(7)
    );
}

// ---------------------------------------------------------------------------
// Reading into an existing value
// ---------------------------------------------------------------------------

#[test]
fn reading_the_same_variant_again_keeps_its_payload() {
    let mut shape = Shape::Label(String::with_capacity(64));
    let before = match &shape {
        Shape::Label(s) => s.capacity(),
        _ => unreachable!(),
    };
    structio::read_into(&mut shape, r#"{"Label":"hello"}"#).unwrap();
    match &shape {
        Shape::Label(s) => {
            assert_eq!(s, "hello");
            // The allocation the destination already held was refilled, not
            // replaced, which is the whole reason reading takes a `&mut`.
            assert_eq!(s.capacity(), before);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn reading_a_different_variant_replaces_the_value() {
    let mut shape = Shape::Label("old".into());
    structio::read_into(&mut shape, r#"{"Sides":9}"#).unwrap();
    assert_eq!(shape, Shape::Sides(9));

    structio::read_into(&mut shape, "\"Empty\"").unwrap();
    assert_eq!(shape, Shape::Empty);
}

// ---------------------------------------------------------------------------
// What the rest of the crate makes of a tag
// ---------------------------------------------------------------------------

#[test]
fn a_tag_transcodes_and_is_walked_like_the_object_it_is() {
    let value = Event::Shape(Shape::Circle(Circle { radius: 3.0 }));
    let bytes = to_beve(&value);

    assert!(structio::validate_beve(&bytes).is_ok());
    assert_eq!(structio::beve_to_json(&bytes).unwrap(), to_string(&value));

    // A pointer reaches through the tag, because the tag is a member name.
    assert_eq!(
        structio::from_beve_at::<f64>(&bytes, "/shape/Circle/radius").unwrap(),
        3.0
    );
    assert_eq!(
        structio::from_beve_at::<Circle>(&bytes, "/shape/Circle").unwrap(),
        Circle { radius: 3.0 }
    );
}

#[test]
fn a_tag_streams_like_any_other_value() {
    let values = vec![Shape::Empty, Shape::Sides(1), Shape::Label("x".into())];
    let mut out = Vec::new();
    structio::to_writer(&values, &mut out).unwrap();
    assert_eq!(
        from_str::<Vec<Shape>>(std::str::from_utf8(&out).unwrap()).unwrap(),
        values
    );

    // One document at a time, at a buffer size that cuts every tag in half.
    let text = values.iter().map(to_string).collect::<Vec<_>>().join("\n");
    let mut docs = structio::Documents::new(
        std::io::Cursor::new(text.into_bytes()),
        structio::Mode::Lines,
    )
    .read_size(1);
    let read: Vec<Shape> = docs.iter::<Shape>().collect::<Result<_, _>>().unwrap();
    assert_eq!(read, values);
}

// ---------------------------------------------------------------------------
// Every prefix and every corruption
// ---------------------------------------------------------------------------

/// One value under Miri rather than four, and every eighth byte rather than
/// every one. Miri interprets rather than executes, at hundreds of times the
/// cost, and every branch this sweep can take is reachable within the first
/// few positions; what it is here to watch is the unsafe in the writers, which
/// one value exercises as well as four.
#[test]
fn no_prefix_or_corruption_of_a_tag_panics() {
    let step = if cfg!(miri) { 8 } else { 1 };
    let values = [
        Event::Connected,
        Event::Log(Level::Warning),
        Event::Shape(Shape::Circle(Circle { radius: -0.5 })),
        Event::Shape(Shape::Corners((1.5, 2.5))),
    ];
    for value in values
        .iter()
        .take(if cfg!(miri) { 1 } else { values.len() })
    {
        let text = to_string(value);
        for cut in 0..text.len() {
            let _ = from_str::<Event>(&text[..cut]);
        }
        for pos in (0..text.len()).step_by(step) {
            let mut bytes = text.clone().into_bytes();
            for byte in 0u8..128 {
                bytes[pos] = byte;
                if let Ok(s) = std::str::from_utf8(&bytes) {
                    let _ = from_str::<Event>(s);
                }
            }
        }

        let bytes = to_beve(value);
        for cut in 0..bytes.len() {
            let _ = from_beve::<Event>(&bytes[..cut]);
        }
        for pos in (0..bytes.len()).step_by(step) {
            let mut damaged = bytes.clone();
            for byte in 0u8..=255 {
                damaged[pos] = byte;
                let _ = from_beve::<Event>(&damaged);
                let _ = structio::validate_beve(&damaged);
                let _ = structio::beve_to_json(&damaged);
            }
        }
    }
}
