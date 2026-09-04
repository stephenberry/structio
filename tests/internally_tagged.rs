//! Internally tagged enums, in both formats.
//!
//! One wire form and no second: an object one of whose members is the tag
//! naming the variant, the rest being that variant's own. What is checked here
//! is that both formats agree, that a round trip is exact, and that a tag
//! which does not come first is found by stepping over the members before it,
//! which are then read after the ones that follow it.

use structio::{
    ErrorCode, Pretty, RequireKeys, SkipNull, SkipUnknown, from_beve, from_str, to_beve, to_string,
    to_string_with,
};

#[derive(Default, PartialEq, Debug, Clone)]
struct Circle {
    radius: f64,
}
structio::object!(Circle { radius });

#[derive(Default, PartialEq, Debug, Clone)]
struct Rect {
    w: f64,
    h: f64,
}
structio::object!(Rect { w, h });

#[derive(Default, PartialEq, Debug, Clone)]
enum Shape {
    #[default]
    Empty,
    Circle(Circle),
    Rect(Rect),
}
structio::tagged_enum!(Shape as tag "kind" {
    Empty,
    Circle(_),
    Rect(_)
});

// -----------------------------------------------------------------------
// The form itself
// -----------------------------------------------------------------------

#[test]
fn a_variant_shares_one_object_with_its_tag() {
    // The point of the whole feature: the payload's members are *beside* the
    // tag, not nested under it the way `tagged_enum!` nests them.
    assert_eq!(
        to_string(&Shape::Circle(Circle { radius: 1.5 })),
        r#"{"kind":"Circle","radius":1.5}"#
    );
    assert_eq!(
        to_string(&Shape::Rect(Rect { w: 2.0, h: 3.0 })),
        r#"{"kind":"Rect","w":2,"h":3}"#
    );
}

#[test]
fn a_variant_carrying_nothing_is_the_tag_alone() {
    assert_eq!(to_string(&Shape::Empty), r#"{"kind":"Empty"}"#);
    assert_eq!(
        from_str::<Shape>(r#"{"kind":"Empty"}"#).unwrap(),
        Shape::Empty
    );
}

#[test]
fn every_variant_round_trips_in_both_formats() {
    for shape in [
        Shape::Empty,
        Shape::Circle(Circle { radius: 0.25 }),
        Shape::Rect(Rect { w: -1.0, h: 4.5 }),
    ] {
        let json = to_string(&shape);
        assert_eq!(from_str::<Shape>(&json).unwrap(), shape, "json {json}");

        let beve = to_beve(&shape);
        assert_eq!(from_beve::<Shape>(&beve).unwrap(), shape, "beve {shape:?}");
    }
}

#[test]
fn the_two_formats_agree_on_which_variant_a_document_names() {
    // Same declaration, same tag, same names: the encodings differ in bytes
    // and in nothing else.
    let shape = Shape::Rect(Rect { w: 7.0, h: 8.0 });
    let from_json: Shape = from_str(&to_string(&shape)).unwrap();
    let from_beve: Shape = from_beve(&to_beve(&shape)).unwrap();
    assert_eq!(from_json, from_beve);
}

// -----------------------------------------------------------------------
// The tag has to come first
// -----------------------------------------------------------------------

#[test]
fn a_late_tag_is_found_and_the_members_before_it_are_read() {
    // Valid documents that put the tag after a member: what a sorted-key
    // writer produces. Both formats read them to the same value as the
    // tag-first form.
    assert_eq!(
        from_str::<Shape>(r#"{"radius":1.5,"kind":"Circle"}"#).unwrap(),
        Shape::Circle(Circle { radius: 1.5 })
    );
    let reordered = to_beve(&LateTag {
        radius: 1.5,
        kind: "Circle".into(),
    });
    assert_eq!(
        from_beve::<Shape>(&reordered).unwrap(),
        Shape::Circle(Circle { radius: 1.5 })
    );
    assert_eq!(
        from_beve::<Shape>(&to_beve(&EarlyTag {
            kind: "Circle".into(),
            radius: 1.5,
        }))
        .unwrap(),
        Shape::Circle(Circle { radius: 1.5 })
    );

    // A tag in the middle: members on both sides of it are read, and a
    // unit variant tolerates members around its tag under `SkipUnknown`.
    let middle = structio::value!({"a": 1, "kind": "Circle", "radius": 2.5});
    assert_eq!(
        structio::from_value_with::<structio::SkipUnknown, Shape>(&middle).unwrap(),
        Shape::Circle(Circle { radius: 2.5 })
    );
    assert_eq!(
        structio::from_str_with::<structio::SkipUnknown, Shape>(
            r#"{"x":[1,{"kind":"y"}],"kind":"Empty","z":null}"#
        )
        .unwrap(),
        Shape::Empty
    );
    assert_eq!(
        structio::from_beve_with::<structio::SkipUnknown, Shape>(&structio::to_beve(&middle))
            .unwrap(),
        Shape::Circle(Circle { radius: 2.5 })
    );

    // Members before the tag still meet the policy: under `Standard` an
    // unknown one is refused, and a required one that is missing is missed.
    assert_eq!(
        from_str::<Shape>(r#"{"a":1,"kind":"Circle","radius":2.5}"#)
            .unwrap_err()
            .code,
        ErrorCode::UnknownKey
    );
    assert_eq!(
        from_str::<Shape>(r#"{"a":1,"kind":"Empty"}"#)
            .unwrap_err()
            .code,
        ErrorCode::UnknownKey
    );

    // No tag anywhere is still `ExpectedTag`, against the first key.
    assert_eq!(
        from_str::<Shape>(r#"{"radius":1.5}"#).unwrap_err().code,
        ErrorCode::ExpectedTag
    );
    assert_eq!(
        from_beve::<Shape>(&to_beve(&structio::value!({"radius": 1.5})))
            .unwrap_err()
            .code,
        ErrorCode::ExpectedTag
    );
}

#[derive(Default)]
struct LateTag {
    radius: f64,
    kind: String,
}
structio::object!(LateTag { radius, kind });

#[derive(Default)]
struct EarlyTag {
    kind: String,
    radius: f64,
}
structio::object!(EarlyTag { kind, radius });

#[derive(Default, PartialEq, Debug)]
struct Frame {
    // `a` sorts before the tag and `z` after it, so a sorted-key writer puts
    // the shape, whose own tag is late, on the far side of this tag.
    a: u32,
    z: Shape,
}
structio::object!(Frame { a, z });

#[derive(PartialEq, Debug)]
enum Framed {
    Frame(Frame),
}
impl Default for Framed {
    fn default() -> Self {
        Framed::Frame(Frame::default())
    }
}
structio::tagged_enum!(Framed as tag "kind" { Frame(_) });

#[test]
fn a_late_tag_inside_a_late_tag_loses_nothing() {
    // Each object's members before its tag are held while the payload is
    // read, and the payload holds an object in the same state. The outer
    // run has to survive the inner one being found and read.
    let want = Framed::Frame(Frame {
        a: 1,
        z: Shape::Circle(Circle { radius: 2.5 }),
    });
    let sorted =
        structio::value!({"a": 1, "kind": "Frame", "z": {"kind": "Circle", "radius": 2.5}});
    assert_eq!(
        sorted.to_string(),
        r#"{"a":1,"kind":"Frame","z":{"kind":"Circle","radius":2.5}}"#
    );
    assert_eq!(from_str::<Framed>(&sorted.to_string()).unwrap(), want);
    assert_eq!(from_beve::<Framed>(&sorted.to_beve()).unwrap(), want);
    // The inner tag late as well.
    let text = r#"{"a":1,"kind":"Frame","z":{"radius":2.5,"kind":"Circle"}}"#;
    assert_eq!(from_str::<Framed>(text).unwrap(), want);
    let inner_late =
        structio::value!({"a": 1, "kind": "Frame", "z": {"radius": 2.5, "kind": "Circle"}});
    assert_eq!(from_beve::<Framed>(&inner_late.to_beve()).unwrap(), want);
    // Three deep, every tag last.
    let text = r#"{"a":1,"z":{"radius":2.5,"kind":"Circle"},"kind":"Frame"}"#;
    assert_eq!(from_str::<Framed>(text).unwrap(), want);
}

#[test]
fn a_key_on_both_sides_of_a_late_tag_keeps_the_earlier_value() {
    // The members before the tag are read last, so the earlier duplicate
    // overwrites the later one; with the tag first, the later one wins.
    assert_eq!(
        from_str::<Shape>(r#"{"radius":1,"kind":"Circle","radius":2}"#).unwrap(),
        Shape::Circle(Circle { radius: 1.0 })
    );
    assert_eq!(
        from_str::<Shape>(r#"{"kind":"Circle","radius":1,"radius":2}"#).unwrap(),
        Shape::Circle(Circle { radius: 2.0 })
    );
}

#[test]
fn an_object_with_no_tag_at_all_is_the_same_refusal() {
    for doc in [r#"{}"#, r#"{"radius":1.5}"#] {
        assert_eq!(
            from_str::<Shape>(doc).unwrap_err().code,
            ErrorCode::ExpectedTag,
            "{doc}"
        );
    }
    // An empty object is reported against itself, there being no member to
    // point at.
    assert_eq!(from_str::<Shape>("{}").unwrap_err().index, 0);
}

#[test]
fn a_tag_whose_value_is_not_a_name_is_refused() {
    // The key was right and what was under it cannot name a variant. Not an
    // unknown variant: nothing was named at all.
    for doc in [
        r#"{"kind":5}"#,
        r#"{"kind":null}"#,
        r#"{"kind":{"a":1}}"#,
        r#"{"kind":["Circle"]}"#,
    ] {
        assert_eq!(
            from_str::<Shape>(doc).unwrap_err().code,
            ErrorCode::ExpectedTag,
            "{doc}"
        );
    }
}

#[test]
fn a_value_that_is_not_an_object_is_not_an_internally_tagged_enum() {
    // Unlike `tagged_enum!`, there is no bare-name form to fall back on: the
    // tag lives inside an object, so there has to be one.
    assert_eq!(
        from_str::<Shape>(r#""Empty""#).unwrap_err().code,
        ErrorCode::ExpectedBrace
    );
    assert_eq!(
        from_beve::<Shape>(&to_beve(&"Empty")).unwrap_err().code,
        ErrorCode::ExpectedObject
    );
}

// -----------------------------------------------------------------------
// Names
// -----------------------------------------------------------------------

#[test]
fn a_tag_that_is_first_and_names_nothing_is_an_unknown_variant() {
    // The distinction the two codes draw: `ExpectedTag` is "the tag was not
    // where it has to be", `UnknownVariant` is "it was, and named nothing".
    let err = from_str::<Shape>(r#"{"kind":"Triangle"}"#).unwrap_err();
    assert_eq!(err.code, ErrorCode::UnknownVariant);
    assert_eq!(
        from_beve::<Shape>(&to_beve(&Untagged {
            kind: "Triangle".into()
        }))
        .unwrap_err()
        .code,
        ErrorCode::UnknownVariant
    );
}

#[derive(Default)]
struct Untagged {
    kind: String,
}
structio::object!(Untagged { kind });

#[test]
fn an_unknown_variant_is_refused_under_every_policy() {
    // A member with nowhere to go can be stepped over; a variant with nowhere
    // to go leaves the value undecided. Same rule as `tagged_enum!`.
    let err =
        structio::json::from_str_with::<SkipUnknown, Shape>(r#"{"kind":"Triangle"}"#).unwrap_err();
    assert_eq!(err.code, ErrorCode::UnknownVariant);
}

#[derive(Default, PartialEq, Debug)]
struct Path {
    path: String,
}
structio::object!(Path { path });

#[derive(Default, PartialEq, Debug)]
enum Op {
    #[default]
    Noop,
    ReadFile(Path),
}
structio::tagged_enum!(Op as "kebab-case" tag "op" {
    Noop,
    ReadFile(_)
});

#[test]
fn a_case_rule_renames_the_variants_and_leaves_the_tag_alone() {
    // The tag names a member of the document; the variants name values of it.
    // A rule that converted both would rename the key a user wrote out by
    // hand.
    assert_eq!(to_string(&Op::Noop), r#"{"op":"noop"}"#);
    assert_eq!(
        to_string(&Op::ReadFile(Path {
            path: "/tmp".into()
        })),
        r#"{"op":"read-file","path":"/tmp"}"#
    );
    assert_eq!(
        from_str::<Op>(r#"{"op":"read-file","path":"/tmp"}"#).unwrap(),
        Op::ReadFile(Path {
            path: "/tmp".into()
        })
    );
}

#[derive(Default, PartialEq, Debug)]
enum Renamed {
    #[default]
    A,
    B(Circle),
}
structio::tagged_enum!(Renamed as tag "t" {
    "alpha" => A,
    "beta" => B(_)
});

#[test]
fn a_per_variant_literal_names_it_on_the_wire() {
    assert_eq!(to_string(&Renamed::A), r#"{"t":"alpha"}"#);
    assert_eq!(
        to_string(&Renamed::B(Circle { radius: 1.0 })),
        r#"{"t":"beta","radius":1}"#
    );
    assert_eq!(from_str::<Renamed>(r#"{"t":"alpha"}"#).unwrap(), Renamed::A);
}

// -----------------------------------------------------------------------
// The payload is an object like any other
// -----------------------------------------------------------------------

#[test]
fn an_unknown_member_beside_the_tag_meets_the_reader_s_policy() {
    let doc = r#"{"kind":"Circle","radius":1.5,"extra":true}"#;
    assert_eq!(
        from_str::<Shape>(doc).unwrap_err().code,
        ErrorCode::UnknownKey,
        "refused under the default policy"
    );
    assert_eq!(
        structio::json::from_str_with::<SkipUnknown, Shape>(doc).unwrap(),
        Shape::Circle(Circle { radius: 1.5 }),
        "stepped over under SkipUnknown"
    );
}

#[test]
fn a_member_beside_a_variant_that_carries_nothing_meets_the_same_policy() {
    // The tag was the whole value, so anything else is an unknown member and
    // is governed like one rather than being ignored outright.
    let doc = r#"{"kind":"Empty","extra":1}"#;
    assert_eq!(
        from_str::<Shape>(doc).unwrap_err().code,
        ErrorCode::UnknownKey
    );
    assert_eq!(
        structio::json::from_str_with::<SkipUnknown, Shape>(doc).unwrap(),
        Shape::Empty
    );

    let beve = to_beve(&TwoMembers {
        kind: "Empty".into(),
        extra: 1,
    });
    assert_eq!(
        from_beve::<Shape>(&beve).unwrap_err().code,
        ErrorCode::UnknownKey
    );
    assert_eq!(
        structio::beve::from_slice_with::<SkipUnknown, Shape>(&beve).unwrap(),
        Shape::Empty
    );
}

#[derive(Default)]
struct TwoMembers {
    kind: String,
    extra: u32,
}
structio::object!(TwoMembers { kind, extra });

#[test]
fn a_missing_payload_member_is_reported_against_the_whole_object() {
    // `RequireKeys` reaches the payload, and the object it names is the one
    // that carried the tag: there is no inner object to point at.
    let err = structio::json::from_str_with::<RequireKeys, Shape>(r#"{"kind":"Rect","w":1}"#)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::MissingKey);
    assert_eq!(err.key, Some("h"));
    assert_eq!(err.index, 0, "the opening brace of the tagged object");
}

#[test]
fn reading_reuses_the_payload_already_held() {
    // Same property `tagged_enum!` has: reading the variant a value already is
    // keeps its buffers rather than replacing the value.
    let mut op = Op::ReadFile(Path {
        path: String::with_capacity(64),
    });
    let before = match &op {
        Op::ReadFile(p) => p.path.capacity(),
        _ => unreachable!(),
    };
    assert!(before >= 64);
    structio::json::read_into(&mut op, r#"{"op":"read-file","path":"/x"}"#).unwrap();
    match &op {
        Op::ReadFile(p) => {
            assert_eq!(p.path, "/x");
            assert_eq!(p.path.capacity(), before, "the buffer survived the read");
        }
        _ => unreachable!(),
    }
}

// -----------------------------------------------------------------------
// Policies that change what is written
// -----------------------------------------------------------------------

#[derive(Default, PartialEq, Debug)]
struct Maybe {
    a: Option<u32>,
    b: u32,
}
structio::object!(Maybe { a, b });

#[derive(Default, PartialEq, Debug)]
enum Holder {
    #[default]
    None,
    Some(Maybe),
}
structio::tagged_enum!(Holder as tag "k" { None, Some(_) });

#[test]
fn skip_null_drops_a_payload_member_and_keeps_the_tag() {
    // The count BEVE writes up front has to agree with the members that follow
    // it, and the tag is one of them. A debug build asserts this, so a
    // mismatch is a panic here rather than a corrupt document.
    let value = Holder::Some(Maybe { a: None, b: 7 });
    assert_eq!(
        to_string_with::<SkipNull, _>(&value),
        r#"{"k":"Some","b":7}"#
    );

    let beve = structio::beve::to_vec_with::<SkipNull, _>(&value);
    assert_eq!(
        structio::beve::from_slice_with::<SkipUnknown, Holder>(&beve).unwrap(),
        Holder::Some(Maybe { a: None, b: 7 })
    );
}

#[test]
fn pretty_printing_puts_the_tag_on_its_own_line_like_any_member() {
    assert_eq!(
        to_string_with::<Pretty, _>(&Shape::Circle(Circle { radius: 1.0 })),
        "{\n  \"kind\": \"Circle\",\n  \"radius\": 1\n}"
    );
    assert_eq!(
        to_string_with::<Pretty, _>(&Shape::Empty),
        "{\n  \"kind\": \"Empty\"\n}"
    );
}

// -----------------------------------------------------------------------
// Nesting
// -----------------------------------------------------------------------

#[derive(Default, PartialEq, Debug)]
struct Drawing {
    name: String,
    shape: Shape,
}
structio::object!(Drawing { name, shape });

#[test]
fn an_internally_tagged_value_nests_as_a_field() {
    let d = Drawing {
        name: "d".into(),
        shape: Shape::Circle(Circle { radius: 2.0 }),
    };
    assert_eq!(
        to_string(&d),
        r#"{"name":"d","shape":{"kind":"Circle","radius":2}}"#
    );
    assert_eq!(from_str::<Drawing>(&to_string(&d)).unwrap(), d);
    assert_eq!(from_beve::<Drawing>(&to_beve(&d)).unwrap(), d);
}

#[test]
fn a_sequence_of_them_reads_back_element_for_element() {
    let shapes = vec![
        Shape::Empty,
        Shape::Circle(Circle { radius: 1.0 }),
        Shape::Rect(Rect { w: 1.0, h: 2.0 }),
    ];
    assert_eq!(from_str::<Vec<Shape>>(&to_string(&shapes)).unwrap(), shapes);
    assert_eq!(from_beve::<Vec<Shape>>(&to_beve(&shapes)).unwrap(), shapes);
}

// -----------------------------------------------------------------------
// Whitespace and truncation
// -----------------------------------------------------------------------

#[test]
fn whitespace_around_the_tag_does_not_change_the_reading() {
    assert_eq!(
        from_str::<Shape>("  {  \"kind\" : \"Circle\" , \"radius\" : 1.5  }  ").unwrap(),
        Shape::Circle(Circle { radius: 1.5 })
    );
}

#[test]
fn a_document_that_ends_mid_value_is_not_a_document_that_held_the_wrong_thing() {
    // A truncated document must never be reported as a *schema* error. Saying
    // `ExpectedTag` for `{"ki` would tell the reader their tag is in the wrong
    // place when the real answer is that the bytes ran out.
    for doc in [
        r#"{"ki"#,
        r#"{"kind""#,
        r#"{"kind":"#,
        r#"{"kind": "#,
        r#"{"kind":"Circ"#,
        r#"{"kind":"Circle","radius""#,
    ] {
        let err = from_str::<Shape>(doc).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnexpectedEnd, "{doc}");
    }
}

#[test]
fn the_two_formats_agree_on_a_truncated_document() {
    // The one place the formats could drift: BEVE gets this for free from its
    // length prefixes, JSON has to decide for itself.
    let whole = to_beve(&Shape::Circle(Circle { radius: 1.5 }));
    for cut in 1..whole.len() {
        assert_eq!(
            from_beve::<Shape>(&whole[..cut]).unwrap_err().code,
            ErrorCode::UnexpectedEnd,
            "truncated to {cut} of {}",
            whole.len()
        );
    }
}

// -----------------------------------------------------------------------
// Generics, borrowed payloads, and one format at a time
// -----------------------------------------------------------------------

#[derive(Default, PartialEq, Debug)]
struct Page<T> {
    items: Vec<T>,
}
structio::object!([T: structio::ReadWrite + Default] Page<T> { items });

#[derive(Default, PartialEq, Debug)]
enum Response<T> {
    #[default]
    Empty,
    Page(Page<T>),
}
structio::tagged_enum!([T: structio::ReadWrite + Default] Response<T> as tag "kind" {
    Empty,
    Page(_)
});

#[test]
fn a_generic_payload_is_declared_the_way_a_generic_struct_is() {
    let r = Response::Page(Page {
        items: vec![1u32, 2, 3],
    });
    assert_eq!(to_string(&r), r#"{"kind":"Page","items":[1,2,3]}"#);
    assert_eq!(from_str::<Response<u32>>(&to_string(&r)).unwrap(), r);
    assert_eq!(from_beve::<Response<u32>>(&to_beve(&r)).unwrap(), r);
}

#[derive(Default, PartialEq, Debug)]
struct Borrowed<'a> {
    name: &'a str,
}
structio::object!(['de] Borrowed<'de> { name });

#[derive(Default, PartialEq, Debug)]
enum Event<'a> {
    #[default]
    Tick,
    Named(Borrowed<'a>),
}
structio::tagged_enum!(['de] Event<'de> as tag "kind" { Tick, Named(_) });

/// The same enum under its own lifetime name, through the tag-clause arms.
#[derive(Default, PartialEq, Debug)]
enum NamedEvent<'a> {
    #[default]
    Tick,
    Named(Borrowed<'a>),
}
structio::tagged_enum!(['a] NamedEvent<'a> as tag "kind" { Tick, Named(_) });

#[test]
fn a_tagged_payload_keeps_its_own_lifetime_name() {
    let doc = r#"{"kind":"Named","name":"abc"}"#;
    let e: NamedEvent<'_> = from_str(doc).unwrap();
    let NamedEvent::Named(b) = e else {
        panic!("expected Named")
    };
    assert_eq!(b.name, "abc");
    let at = doc.find("abc").unwrap();
    assert!(std::ptr::eq(b.name.as_ptr(), doc[at..].as_ptr()));

    let beve = structio::to_beve(&NamedEvent::Named(Borrowed { name: "abc" }));
    let back: NamedEvent<'_> = structio::from_beve(&beve).unwrap();
    assert_eq!(back, NamedEvent::Named(Borrowed { name: "abc" }));
}

#[test]
fn a_payload_may_borrow_from_the_document() {
    let doc = r#"{"kind":"Named","name":"abc"}"#;
    let e: Event<'_> = from_str(doc).unwrap();
    match e {
        Event::Named(b) => {
            assert_eq!(b.name, "abc");
            // The borrow is of the document itself, not a copy of it.
            let at = doc.find("abc").unwrap();
            assert!(std::ptr::eq(b.name.as_ptr(), doc[at..].as_ptr()));
        }
        _ => panic!("wrong variant"),
    }
}

#[derive(Default, PartialEq, Debug)]
enum JsonOnly {
    #[default]
    A,
    B(Circle),
}
structio::json_tagged_enum!(JsonOnly as tag "t" { A, B(_) });

#[derive(Default, PartialEq, Debug)]
enum BeveOnly {
    #[default]
    A,
    B(Circle),
}
structio::beve_tagged_enum!(BeveOnly as tag "t" { A, B(_) });

#[test]
fn one_format_at_a_time_generates_only_that_format() {
    // Nothing here checks the *absence* of the other impl, which is a compile
    // error rather than a value; what it checks is that the narrower macros
    // produce a working codec at all.
    assert_eq!(
        to_string(&JsonOnly::B(Circle { radius: 1.0 })),
        r#"{"t":"B","radius":1}"#
    );
    assert_eq!(from_str::<JsonOnly>(r#"{"t":"A"}"#).unwrap(), JsonOnly::A);

    let beve = to_beve(&BeveOnly::B(Circle { radius: 2.0 }));
    assert_eq!(
        from_beve::<BeveOnly>(&beve).unwrap(),
        BeveOnly::B(Circle { radius: 2.0 })
    );
}

// -----------------------------------------------------------------------
// The rest of the crate sees an ordinary object
// -----------------------------------------------------------------------

#[test]
fn validation_pointers_and_transcoding_walk_through_one() {
    // Nothing in the crate's generic machinery needs to know this object is an
    // enum: the tag is a member like any other. That is the practical payoff
    // of internal tagging over external, and it is worth pinning.
    let shape = Shape::Circle(Circle { radius: 1.5 });
    let beve = to_beve(&shape);

    assert!(structio::beve::validate(&beve).is_ok());

    // A pointer reaches straight into the tagged object, tag and payload
    // member alike, with no enum-shaped step in the path.
    assert_eq!(
        structio::from_beve_at::<String>(&beve, "/kind").unwrap(),
        "Circle"
    );
    assert_eq!(
        structio::from_beve_at::<f64>(&beve, "/radius").unwrap(),
        1.5
    );

    // And it transcodes to exactly the JSON the JSON writer produces.
    assert_eq!(structio::beve_to_json(&beve).unwrap(), to_string(&shape));
}

#[derive(Default, PartialEq, Debug)]
struct Wrapper {
    inner: Box<Node>,
}
structio::object!(Wrapper { inner });

#[derive(Default, PartialEq, Debug)]
enum Node {
    #[default]
    Leaf,
    Branch(Wrapper),
}
structio::tagged_enum!(Node as tag "t" { Leaf, Branch(_) });

#[test]
fn nesting_balances_the_depth_counter() {
    // Every `read_internally_tagged` enters a level and the variant's own
    // reader leaves it. If the two ever fell out of step, a document nested
    // deeply enough would either be refused early or blow past the limit;
    // this walks far enough to notice either.
    let mut node = Node::Leaf;
    for _ in 0..60 {
        node = Node::Branch(Wrapper {
            inner: Box::new(node),
        });
    }
    let json = to_string(&node);
    assert_eq!(from_str::<Node>(&json).unwrap(), node);
    assert_eq!(from_beve::<Node>(&to_beve(&node)).unwrap(), node);
}

#[test]
fn a_document_nested_past_the_limit_is_still_refused() {
    // The counter has to keep working, not merely stay balanced.
    let deep = format!(
        "{}{}{}",
        r#"{"t":"Branch","inner":"#.repeat(300),
        r#"{"t":"Leaf"}"#,
        "}".repeat(300)
    );
    assert_eq!(
        from_str::<Node>(&deep).unwrap_err().code,
        ErrorCode::ExceededMaxDepth
    );
}

// -----------------------------------------------------------------------
// The tag may not be a payload's field
// -----------------------------------------------------------------------
//
// The collision itself is a compile error, so it cannot be asserted from here
// without a compile-fail harness this crate does not carry. What these
// pin is the other side: the shapes that look like collisions and are not,
// which is where a check of this kind goes wrong. Each would stop compiling
// if the comparison were made against Rust field names rather than wire keys,
// or if unit variants were checked.

#[derive(Default, PartialEq, Debug)]
struct CamelPayload {
    kind_of: String,
}
structio::object!(CamelPayload as "camelCase" { kind_of });

#[derive(Default, PartialEq, Debug)]
enum CaseSensitive {
    #[default]
    A,
    B(CamelPayload),
}
// The tag is the Rust field's name; the field reaches the wire as `kindOf`,
// so the two do not collide. Were the check comparing pre-conversion names it
// would refuse this.
structio::tagged_enum!(CaseSensitive as tag "kind_of" { A, B(_) });

#[test]
fn a_tag_matching_a_field_s_rust_name_but_not_its_wire_key_is_fine() {
    assert_eq!(
        to_string(&CaseSensitive::B(CamelPayload {
            kind_of: "x".into()
        })),
        r#"{"kind_of":"B","kindOf":"x"}"#
    );
    assert_eq!(
        from_str::<CaseSensitive>(r#"{"kind_of":"B","kindOf":"x"}"#).unwrap(),
        CaseSensitive::B(CamelPayload {
            kind_of: "x".into()
        })
    );
}

#[derive(Default, PartialEq, Debug)]
struct RenamedField {
    kind: String,
}
structio::object!(RenamedField { "k" => kind });

#[derive(Default, PartialEq, Debug)]
enum MovedOff {
    #[default]
    A,
    B(RenamedField),
}
// A per-field rename moves the field off the tag, so `kind` is free again.
structio::tagged_enum!(MovedOff as tag "kind" { A, B(_) });

#[test]
fn a_field_renamed_off_the_tag_frees_the_name() {
    assert_eq!(
        to_string(&MovedOff::B(RenamedField { kind: "y".into() })),
        r#"{"kind":"B","k":"y"}"#
    );
}

#[derive(Default, PartialEq, Debug)]
enum AllUnits {
    #[default]
    Kind,
    Other,
}
// Every variant carries nothing, so none of them shares its object with any
// member and the tag may be named whatever it likes.
structio::tagged_enum!(AllUnits as tag "kind" { Kind, Other });

#[test]
fn a_variant_carrying_nothing_is_never_a_collision() {
    assert_eq!(to_string(&AllUnits::Kind), r#"{"kind":"Kind"}"#);
    assert_eq!(
        from_str::<AllUnits>(r#"{"kind":"Other"}"#).unwrap(),
        AllUnits::Other
    );
}
