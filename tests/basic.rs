//! End to end behavior: the shapes a user actually writes.

use structio::{ErrorCode, SkipUnknown, from_str, from_str_with, to_string};

#[derive(Default, Debug, PartialEq)]
struct Person {
    first_name: String,
    age: u32,
    active: bool,
    scores: Vec<f64>,
}
structio::object!(Person {
    first_name,
    age,
    active,
    scores
});

#[test]
fn roundtrip_simple() {
    let json = r#"{"first_name":"Ada","age":36,"active":true,"scores":[1.5,2,3.25]}"#;
    let p: Person = from_str(json).unwrap();
    assert_eq!(
        p,
        Person {
            first_name: "Ada".into(),
            age: 36,
            active: true,
            scores: vec![1.5, 2.0, 3.25],
        }
    );
    assert_eq!(to_string(&p), json);
}

#[test]
fn keys_may_arrive_in_any_order() {
    let json = r#"{"scores":[],"active":false,"age":1,"first_name":"x"}"#;
    let p: Person = from_str(json).unwrap();
    assert_eq!(p.first_name, "x");
    assert_eq!(p.age, 1);
    assert!(!p.active);
    assert!(p.scores.is_empty());
}

#[test]
fn unknown_keys_are_refused_by_default() {
    let err = from_str::<Person>(r#"{"age":7,"unknown":"str"}"#).unwrap_err();
    assert_eq!(err.code, ErrorCode::UnknownKey);
    // The position is the key itself, not the brace it followed nor the value
    // it would have introduced.
    assert_eq!(err.index, r#"{"age":7,""#.len());
}

#[test]
fn unknown_keys_are_skipped_under_skip_unknown() {
    let json = r#"{"zzz":{"a":[1,2,{"b":null}]},"age":7,"unknown":"str","first_name":"q",
                   "active":true,"scores":[1],"trailing":[[[]]]}"#;
    let p = from_str_with::<SkipUnknown, Person>(json).unwrap();
    assert_eq!(p.age, 7);
    assert_eq!(p.first_name, "q");
}

#[test]
fn missing_keys_keep_their_defaults() {
    let p: Person = from_str(r#"{"age":3}"#).unwrap();
    assert_eq!(p.age, 3);
    assert_eq!(p.first_name, "");
    assert!(p.scores.is_empty());
}

#[test]
fn whitespace_everywhere() {
    let json = "  {\n \"age\" : 5 ,\t\"first_name\"\r\n:\"z\", \"active\" : false , \"scores\" : [ 1 , 2 ] }  ";
    let p: Person = from_str(json).unwrap();
    assert_eq!(p.age, 5);
    assert_eq!(p.first_name, "z");
    assert_eq!(p.scores, vec![1.0, 2.0]);
}

// --- renamed keys ----------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Renamed {
    first_name: String,
    id: u64,
}
structio::object!(Renamed {
    "first-name" => first_name,
    "ID" => id,
});

#[test]
fn renamed_keys() {
    let json = r#"{"first-name":"Ada","ID":9}"#;
    let r: Renamed = from_str(json).unwrap();
    assert_eq!(r.first_name, "Ada");
    assert_eq!(r.id, 9);
    assert_eq!(to_string(&r), json);
}

// --- nesting ---------------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Inner {
    x: i32,
    y: i32,
}
structio::object!(Inner { x, y });

#[derive(Default, Debug, PartialEq)]
struct Outer {
    label: String,
    point: Inner,
    points: Vec<Inner>,
    maybe: Option<Inner>,
}
structio::object!(Outer {
    label,
    point,
    points,
    maybe
});

#[test]
fn nested_structs() {
    let json = r#"{"label":"L","point":{"x":1,"y":2},"points":[{"x":3,"y":4}],"maybe":null}"#;
    let o: Outer = from_str(json).unwrap();
    assert_eq!(o.point, Inner { x: 1, y: 2 });
    assert_eq!(o.points, vec![Inner { x: 3, y: 4 }]);
    assert_eq!(o.maybe, None);
    assert_eq!(to_string(&o), json);

    let json = r#"{"label":"L","point":{"x":1,"y":2},"points":[],"maybe":{"x":5,"y":6}}"#;
    let o: Outer = from_str(json).unwrap();
    assert_eq!(o.maybe, Some(Inner { x: 5, y: 6 }));
    assert_eq!(to_string(&o), json);
}

// --- borrowed fields -------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Borrowed<'a> {
    name: &'a str,
    tag: std::borrow::Cow<'a, str>,
}
structio::object!(['de] Borrowed<'de> { name, tag });

#[test]
fn borrowed_strings_do_not_copy() {
    let json = String::from(r#"{"name":"zero copy","tag":"plain"}"#);
    let b: Borrowed = from_str(&json).unwrap();
    assert_eq!(b.name, "zero copy");
    // The borrow points into the original document, not a new allocation.
    assert!(std::ptr::eq(b.name.as_ptr(), json[9..].as_ptr()));
    assert!(matches!(b.tag, std::borrow::Cow::Borrowed(_)));
}

#[test]
fn borrowed_str_refuses_escapes_rather_than_allocating() {
    let json = r#"{"name":"a\nb","tag":"x"}"#;
    let err = from_str::<Borrowed>(json).unwrap_err();
    assert_eq!(err.code, ErrorCode::EscapeInBorrowedString);

    // `Cow` accepts them, and becomes owned.
    let json = r#"{"name":"ok","tag":"a\nb"}"#;
    let b: Borrowed = from_str(json).unwrap();
    assert_eq!(b.tag, "a\nb");
    assert!(matches!(b.tag, std::borrow::Cow::Owned(_)));
}

// --- generics --------------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Page<T> {
    items: Vec<T>,
    cursor: Option<String>,
}
structio::object!([T: structio::ReadWrite + Default] Page<T> { items, cursor });

#[test]
fn generic_struct() {
    let json = r#"{"items":[1,2,3],"cursor":"next"}"#;
    let p: Page<i32> = from_str(json).unwrap();
    assert_eq!(p.items, vec![1, 2, 3]);
    assert_eq!(p.cursor.as_deref(), Some("next"));
    assert_eq!(to_string(&p), json);
}

// --- strings and escapes ---------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Text {
    s: String,
}
structio::object!(Text { s });

#[test]
fn escapes() {
    let cases = [
        (r#""a\nb""#, "a\nb"),
        (r#""\"\\\/\b\f\n\r\t""#, "\"\\/\u{8}\u{c}\n\r\t"),
        (r#""\u0041\u00e9\u4e2d""#, "Aé中"),
        (r#""\ud83d\ude00""#, "😀"),
        (r#""plain""#, "plain"),
        (r#""""#, ""),
    ];
    for (json, want) in cases {
        let t: Text = from_str(&format!(r#"{{"s":{json}}}"#)).unwrap();
        assert_eq!(t.s, want, "parsing {json}");
    }
}

#[test]
fn escapes_round_trip() {
    for s in [
        "a\nb",
        "\"quoted\"",
        "back\\slash",
        "tab\there",
        "\u{1}\u{1f}",
        "😀 é 中",
    ] {
        let t = Text { s: s.to_string() };
        let json = to_string(&t);
        let back: Text = from_str(&json).unwrap();
        assert_eq!(back.s, s, "round trip via {json}");
    }
}

#[test]
fn bad_escapes_are_rejected() {
    for json in [
        r#"{"s":"\ud800"}"#,        // lone high surrogate
        r#"{"s":"\udc00"}"#,        // lone low surrogate
        r#"{"s":"\ud800\u0041"}"#,  // high surrogate not followed by a low one
        r#"{"s":"\x"}"#,            // not an escape
        r#"{"s":"\u12"}"#,          // truncated
        "{\"s\":\"raw\ncontrol\"}", // unescaped control character
    ] {
        assert!(from_str::<Text>(json).is_err(), "should reject {json}");
    }
}

// --- containers ------------------------------------------------------------

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

#[derive(Default, Debug, PartialEq)]
struct Containers {
    map: BTreeMap<String, i32>,
    ints: BTreeMap<u32, String>,
    set: BTreeSet<i32>,
    deque: VecDeque<u8>,
    fixed: [i16; 3],
    pair: (i32, String),
    boxed: Box<i64>,
}
structio::object!(Containers {
    map,
    ints,
    set,
    deque,
    fixed,
    pair,
    boxed
});

#[test]
fn containers_round_trip() {
    let json = r#"{"map":{"a":1,"b":2},"ints":{"7":"seven"},"set":[1,2,3],"deque":[9,8],"fixed":[1,2,3],"pair":[5,"five"],"boxed":-3}"#;
    let c: Containers = from_str(json).unwrap();
    assert_eq!(c.map.get("b"), Some(&2));
    assert_eq!(c.ints.get(&7).map(String::as_str), Some("seven"));
    assert_eq!(c.set, BTreeSet::from([1, 2, 3]));
    assert_eq!(c.deque, VecDeque::from([9, 8]));
    assert_eq!(c.fixed, [1, 2, 3]);
    assert_eq!(c.pair, (5, "five".to_string()));
    assert_eq!(*c.boxed, -3);
    assert_eq!(to_string(&c), json);
}

#[test]
fn hash_map_round_trips() {
    let mut m: HashMap<String, Vec<i32>> = HashMap::new();
    m.insert("k".into(), vec![1, 2]);
    let json = to_string(&m);
    let back: HashMap<String, Vec<i32>> = from_str(&json).unwrap();
    assert_eq!(back, m);
}

#[test]
fn wrong_array_length_is_an_error() {
    assert!(from_str::<Containers>(r#"{"fixed":[1,2]}"#).is_err());
    assert!(from_str::<Containers>(r#"{"fixed":[1,2,3,4]}"#).is_err());
    assert!(from_str::<Containers>(r#"{"pair":[1]}"#).is_err());
}

// --- errors ----------------------------------------------------------------

#[test]
fn malformed_input_is_rejected() {
    let cases: &[(&str, ErrorCode)] = &[
        ("", ErrorCode::UnexpectedEnd),
        ("{", ErrorCode::UnexpectedEnd),
        (r#"{"age":1"#, ErrorCode::UnexpectedEnd),
        (r#"{"age":}"#, ErrorCode::ExpectedNumber),
        (r#"{"age" 1}"#, ErrorCode::ExpectedColon),
        (r#"{"age":1,}"#, ErrorCode::ExpectedQuote),
        (r#"{"age":01}"#, ErrorCode::InvalidNumber),
        (r#"{"age":1.5}"#, ErrorCode::InvalidNumber),
        (r#"{"age":-1}"#, ErrorCode::NumberOutOfRange),
        (r#"{"age":4294967296}"#, ErrorCode::NumberOutOfRange),
        (r#"{"active":tru}"#, ErrorCode::ExpectedTrue),
        (r#"{"age":1} trailing"#, ErrorCode::TrailingContent),
        ("[1,2]", ErrorCode::ExpectedBrace),
    ];
    for (json, want) in cases {
        let err = from_str::<Person>(json).unwrap_err();
        assert_eq!(err.code, *want, "for input {json:?}");
    }
}

#[test]
fn deep_nesting_is_bounded() {
    // An unknown key, so the nesting is walked by the value skipper.
    let deep = format!("{{\"unknown\":{}{}}}", "[".repeat(400), "]".repeat(400));
    let err = from_str_with::<SkipUnknown, Person>(&deep).unwrap_err();
    assert_eq!(err.code, ErrorCode::ExceededMaxDepth);
}

#[test]
fn errors_carry_a_useful_location() {
    let json = "{\n  \"age\": 1,\n  \"active\": tru\n}";
    let err = from_str::<Person>(json).unwrap_err();
    let rendered = err.display_with(json);
    assert!(rendered.contains("line 3"), "{rendered}");
    assert!(rendered.contains('^'), "{rendered}");
}

// --- schema edge cases -----------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Empty {}
structio::object!(Empty {});

#[derive(Default, Debug, PartialEq)]
struct Single {
    only: i32,
}
structio::object!(Single { only });

#[test]
fn empty_schema() {
    assert_eq!(to_string(&Empty {}), "{}");
    assert_eq!(from_str::<Empty>("{}").unwrap(), Empty {});
    // Every key is unknown, so under the default every one of them is refused.
    assert_eq!(
        from_str::<Empty>(r#"{"a":1,"b":[{"c":null}]}"#)
            .unwrap_err()
            .code,
        ErrorCode::UnknownKey
    );
    assert_eq!(
        from_str_with::<SkipUnknown, Empty>(r#"{"a":1,"b":[{"c":null}]}"#).unwrap(),
        Empty {}
    );
}

#[test]
fn single_field_schema() {
    assert_eq!(to_string(&Single { only: 7 }), r#"{"only":7}"#);
    assert_eq!(from_str::<Single>(r#"{"only":7}"#).unwrap().only, 7);
    // A single-element map still has to confirm the key, or any key at all
    // would match the one field.
    assert_eq!(
        from_str_with::<SkipUnknown, Single>(r#"{"other":7}"#)
            .unwrap()
            .only,
        0
    );
    assert_eq!(
        from_str_with::<SkipUnknown, Single>(r#"{"onl":7,"only":9}"#)
            .unwrap()
            .only,
        9
    );
}

#[test]
fn a_key_that_collides_with_a_real_one_is_still_rejected() {
    // The hash only proposes a candidate; these share the distinguishing byte
    // with a real key but are not equal to it.
    for json in [
        r#"{"first_nameX":"x","age":1}"#,
        r#"{"first_nam":"x","age":1}"#,
        r#"{"f":"x","age":1}"#,
        r#"{"":"x","age":1}"#,
    ] {
        let p = from_str_with::<SkipUnknown, Person>(json).unwrap();
        assert_eq!(p.first_name, "", "should not have matched in {json}");
        assert_eq!(p.age, 1);
    }
}

#[test]
fn duplicate_keys_in_the_document_take_the_last() {
    let p: Person = from_str(r#"{"age":1,"age":2,"age":3}"#).unwrap();
    assert_eq!(p.age, 3);
}

#[test]
fn reading_shrinks_containers_that_were_longer() {
    let mut p = Person {
        scores: vec![1.0, 2.0, 3.0, 4.0],
        first_name: "long previous value".into(),
        ..Default::default()
    };
    structio::read_into(&mut p, r#"{"scores":[9],"first_name":"x"}"#).unwrap();
    assert_eq!(p.scores, vec![9.0]);
    assert_eq!(p.first_name, "x");
}

#[test]
fn borrowed_and_owned_agree() {
    // The same document through the borrowing and the owning paths.
    let json = r#"{"name":"plain text","tag":"also plain"}"#;
    let b: Borrowed = from_str(json).unwrap();
    assert_eq!(b.name, "plain text");
    assert_eq!(to_string(&b), json);
}

#[test]
fn from_slice_validates_utf8() {
    let good = br#"{"age":1}"#;
    assert_eq!(structio::from_slice::<Person>(good).unwrap().age, 1);

    let bad = b"{\"first_name\":\"\xFF\xFE\"}";
    let err = structio::from_slice::<Person>(bad).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidUtf8);
}

#[test]
fn write_into_reuses_the_buffer() {
    let mut buf = String::with_capacity(256);
    let ptr = buf.as_ptr();
    let p = Person {
        age: 1,
        ..Default::default()
    };
    for _ in 0..10 {
        structio::write_into(&p, &mut buf);
    }
    assert!(buf.contains(r#""age":1"#));
    // The allocation survived every call.
    assert!(std::ptr::eq(buf.as_ptr(), ptr));
}

#[test]
fn leading_whitespace_before_any_top_level_value() {
    // Containers skipped it themselves; scalars did not, so a document that
    // opened with a space or a newline failed at byte 0.
    assert_eq!(from_str::<i32>(" 1").unwrap(), 1);
    assert!(from_str::<bool>("\n true").unwrap());
    assert_eq!(from_str::<String>("\t \"x\"").unwrap(), "x");
    assert_eq!(from_str::<f64>("\r\n 1.5").unwrap(), 1.5);
    assert_eq!(from_str::<Option<i32>>("  null").unwrap(), None);
    assert_eq!(from_str::<Vec<i32>>(" [1]").unwrap(), vec![1]);
    assert_eq!(from_str::<Person>(" {\"age\":1}").unwrap().age, 1);
    // Trailing whitespace was already fine, and stays fine.
    assert_eq!(from_str::<i32>(" 1 \n").unwrap(), 1);
}

#[test]
fn display_with_survives_a_mismatched_input() {
    // `display_with` takes any `&str`, so the index need not land on a
    // character boundary of it. Rendering a diagnostic must not panic.
    let e = structio::Error::new(ErrorCode::InvalidNumber, 1);
    assert!(e.display_with("\u{e9} hello").contains("invalid number"));
    for i in 0..12 {
        let _ = structio::Error::new(ErrorCode::InvalidNumber, i).display_with("\u{1f389}\u{e9}ab");
    }
}

#[test]
fn caret_lines_up_with_the_reported_column() {
    let doc = "{\"first_name\":\"\u{e9}\u{e9}\u{e9}\", \"age\": xx}";
    let err = from_str::<Person>(doc).unwrap_err();
    let shown = err.display_with(doc);
    let mut lines = shown.lines();
    let header = lines.next().unwrap();
    let source = lines.next().unwrap();
    let caret = lines.next().unwrap();

    let col: usize = header.rsplit("column ").next().unwrap().parse().unwrap();
    // The caret is drawn in characters, so its offset must equal the column
    // the header reports, not the byte offset into a multi-byte line.
    assert_eq!(caret.chars().position(|c| c == '^').unwrap() + 1, col);
    assert_eq!(source.chars().nth(col - 1), Some('x'));
}

#[test]
fn one_long_key_does_not_cost_the_object_its_hash() {
    // Key *length* used to force the whole object to a linear scan, even
    // though only the per-length table is indexed by length.
    #[derive(Default)]
    struct Wide {
        short: u32,
        long: u32,
    }
    const LONG: &str = "llllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllll\
llllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllll\
llllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllll\
llllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllll\
llllllllllllllllllllllllllllllllllllllllllllllll";
    structio::object!(Wide { short, "LONG_KEY" => long });

    assert!(LONG.len() > 256);
    let w: Wide = from_str(r#"{"short":1,"LONG_KEY":2}"#).unwrap();
    assert_eq!((w.short, w.long), (1, 2));
    assert_ne!(
        <Wide as structio::Keys>::MAP.kind,
        structio::keymap::HashKind::Linear
    );
}

#[test]
fn shared_pointers_read_in_place_when_unshared() {
    use std::rc::Rc;
    use std::sync::Arc;

    // A sole owner is read through, like `Box`, so the payload's allocations
    // survive instead of being dropped for a fresh one.
    let mut rc: Rc<String> = Rc::new(String::with_capacity(64));
    let ptr = rc.as_ptr();
    structio::read_into(&mut rc, "\"hello\"").unwrap();
    assert_eq!(&*rc, "hello");
    assert!(std::ptr::eq(rc.as_ptr(), ptr));

    // A shared payload cannot be touched, so it is replaced and the other
    // handle keeps what it had.
    let mut a: Arc<String> = Arc::new("old".to_string());
    let keep = Arc::clone(&a);
    structio::read_into(&mut a, "\"new\"").unwrap();
    assert_eq!(&*a, "new");
    assert_eq!(&*keep, "old");
}

#[test]
fn separator_errors_agree_between_read_and_skip() {
    #[derive(Default, Debug)]
    struct Only {
        a: u32,
    }
    structio::object!(Only { a });

    // The same malformed separator, once in a field we read and once inside a
    // value we skip, must report the same thing.
    let read = from_str::<Only>(r#"{"a":1 "b":2}"#).unwrap_err();
    let skipped = from_str_with::<SkipUnknown, Only>(r#"{"z":{"a":1 "b":2},"a":3}"#).unwrap_err();
    assert_eq!(read.code, ErrorCode::ExpectedComma);
    assert_eq!(skipped.code, ErrorCode::ExpectedComma);
}

/// The BEVE key encoder binds a `const N` for the key's length and passes it as
/// a generic argument. A bare `N` there parses as a *type*, so a user type of
/// that name used to win the lookup and the declaration would not compile at
/// all. Nothing else in the suite declares a struct short enough to collide.
#[test]
fn a_type_named_like_the_macros_own_constant_still_declares() {
    #[derive(Default, Debug, PartialEq)]
    struct N {
        x: u8,
    }
    structio::object!(N { x });

    // The other identifier the macro binds. This one is only ever used as a
    // value, so hygiene already covers it; it is here so that a future edit
    // moving it into type position does not pass unnoticed.
    #[derive(Default, Debug, PartialEq)]
    #[allow(clippy::upper_case_acronyms)]
    struct KEY {
        y: u8,
    }
    structio::object!(KEY { y });

    let n = N { x: 1 };
    assert_eq!(structio::to_string(&n), r#"{"x":1}"#);
    assert_eq!(structio::from_beve::<N>(&structio::to_beve(&n)).unwrap(), n);

    let k = KEY { y: 2 };
    assert_eq!(
        structio::from_beve::<KEY>(&structio::to_beve(&k)).unwrap(),
        k
    );
}
