//! Fields a document has to supply, marked one at a time.
//!
//! [`Options::ERROR_ON_MISSING_KEYS`] answers the question for a whole reading:
//! either every declared member has to be there or none of them does. Real
//! schemas are mixed, so this is the same question asked of the type instead.
//! A `#[required]` field is required under every policy, and a policy that
//! requires everything still does.
//!
//! The interest is in where the two meet: a marked field inside a struct read
//! under the default policy, an unmarked one under `RequireKeys`, a mark that
//! travels into a nested struct nothing else asked anything of, and the wide
//! struct the global option refuses but a mark does not.
//!
//! Two refusals here are compile errors and so cannot be a `#[test]`: a marked
//! field past the 64th, and a marker that is not `required`. Both are checked
//! by hand, this crate having no `trybuild` harness.

use std::collections::BTreeMap;

use structio::{
    Documents, ErrorCode, Keys, RequireKeys, Same, SkipNull, SkipUnknown, beve, from_beve,
    from_beve_with, from_str, from_str_with, read_beve_into, to_beve, to_string_with,
};

/// A mixed schema, which is the case neither global setting fits: two members
/// the format defines and one a document may leave to its default.
#[derive(Debug, Default, PartialEq)]
struct Asset {
    version: String,
    min_version: u32,
    generator: String,
}

structio::object!(Asset {
    #[required]
    version,
    #[required]
    "minVersion" => min_version,
    generator,
});

/// The same shape marking nothing, so that every claim about the marked one can
/// be checked against the behavior it changed.
#[derive(Debug, Default, PartialEq)]
struct Loose {
    version: String,
    min_version: u32,
    generator: String,
}

structio::object!(Loose {
    version,
    "minVersion" => min_version,
    generator,
});

fn asset() -> Asset {
    Asset {
        version: "2.0".into(),
        min_version: 1,
        generator: "g".into(),
    }
}

/// A document holding both marked members and neither of the rest.
const MINIMAL: &str = r#"{"version":"2.0","minVersion":1}"#;

// A writer puts down every member its type declares, so a BEVE document short
// of one has to come from a type that does not declare it. These are the three
// ways an `Asset` document can be incomplete.

#[derive(Default)]
struct NoVersion {
    min_version: u32,
    generator: String,
}
structio::object!(NoVersion { "minVersion" => min_version, generator });

#[derive(Default)]
struct NoMinVersion {
    version: String,
    generator: String,
}
structio::object!(NoMinVersion { version, generator });

#[derive(Default)]
struct OnlyGenerator {
    generator: String,
}
structio::object!(OnlyGenerator { generator });

/// The complete document minus the member nothing marked, which is the one an
/// `Asset` has to accept.
#[derive(Default)]
struct NoGenerator {
    version: String,
    min_version: u32,
}
structio::object!(NoGenerator { version, "minVersion" => min_version });

// ---------------------------------------------------------------------------
// The mask
// ---------------------------------------------------------------------------

/// Bit `i` is field `i` of the declaration, not of the struct and not of the
/// key hash: the readers set the same bits from the index the lookup hands
/// back.
#[test]
fn the_mask_names_the_marked_fields_in_declaration_order() {
    assert_eq!(Asset::REQUIRED, 0b011);
    assert_eq!(Loose::REQUIRED, 0);
}

/// Nothing declares itself required by accident. A type that says nothing
/// about it inherits the trait's default, which is the reading every version of
/// this crate has done.
#[test]
fn a_declaration_that_marks_nothing_requires_nothing() {
    assert!(from_str::<Loose>("{}").is_ok());
    assert!(from_str::<Loose>(r#"{"generator":"g"}"#).is_ok());
    assert!(from_beve::<Loose>(&to_beve(&Loose::default())).is_ok());
}

// ---------------------------------------------------------------------------
// What a mark refuses, and what it does not
// ---------------------------------------------------------------------------

#[test]
fn an_unmarked_field_may_still_be_left_out() {
    let got = from_str::<Asset>(MINIMAL).unwrap();
    assert_eq!(got.version, "2.0");
    assert_eq!(got.min_version, 1);
    assert_eq!(got.generator, "");

    let doc = to_beve(&NoGenerator {
        version: "2.0".into(),
        min_version: 1,
    });
    let got = from_beve::<Asset>(&doc).unwrap();
    assert_eq!(got.version, "2.0");
    assert_eq!(got.generator, "");
}

/// One case per marked field, since a mask that dropped a bit would still
/// refuse the document that leaves out the other.
#[test]
fn a_marked_field_left_out_is_a_missing_key() {
    for json in [
        r#"{"minVersion":1,"generator":"g"}"#,
        r#"{"version":"2.0","generator":"g"}"#,
        r#"{"generator":"g"}"#,
        "{}",
    ] {
        assert_eq!(
            from_str::<Asset>(json).unwrap_err().code,
            ErrorCode::MissingKey,
            "for {json:?}"
        );
    }
}

/// The same four documents in BEVE, which the reader reaches by a different
/// route: a member count rather than a closing brace, and a sized key rather
/// than a quoted one.
#[test]
fn a_marked_field_left_out_of_a_beve_object_is_a_missing_key() {
    let docs = [
        to_beve(&NoVersion::default()),
        to_beve(&NoMinVersion::default()),
        to_beve(&OnlyGenerator::default()),
        to_beve(&BTreeMap::<String, u32>::new()),
    ];
    for (i, doc) in docs.iter().enumerate() {
        assert_eq!(
            from_beve::<Asset>(doc).unwrap_err().code,
            ErrorCode::MissingKey,
            "for document {i}"
        );
    }

    // And a complete one still reads, so the refusals above are about what was
    // absent rather than about the format.
    assert_eq!(from_beve::<Asset>(&to_beve(&asset())).unwrap(), asset());
}

/// A complete document satisfies the marks whatever order the members arrive
/// in, and an unknown key stepped over on the way does not disturb the count.
#[test]
fn the_marked_fields_may_arrive_in_any_order() {
    for json in [
        r#"{"version":"2.0","minVersion":1,"generator":"g"}"#,
        r#"{"generator":"g","minVersion":1,"version":"2.0"}"#,
        r#"{"minVersion":1,"generator":"g","version":"2.0"}"#,
    ] {
        assert_eq!(from_str::<Asset>(json).unwrap(), asset(), "for {json:?}");
    }
}

/// The object is what is incomplete, so the position is the brace that opened
/// it rather than the byte that closed it -- the promise
/// [`ErrorCode::MissingKey`] already made under the policy, now made where no
/// policy asked for anything.
#[test]
fn the_error_is_located_at_the_object() {
    let err = from_str::<Asset>(r#"{"version":"2.0"}"#).unwrap_err();
    assert_eq!(err.code, ErrorCode::MissingKey);
    assert_eq!(err.index, 0);

    let err = from_beve::<Asset>(&to_beve(&OnlyGenerator::default())).unwrap_err();
    assert_eq!(err.code, ErrorCode::MissingKey);
    assert_eq!(err.index, 0);
}

// ---------------------------------------------------------------------------
// Where a mark travels
// ---------------------------------------------------------------------------

/// A mark belongs to the type, so it reaches a struct read as somebody else's
/// member without the outer declaration mentioning it.
#[test]
fn a_nested_struct_carries_its_own_requirement() {
    #[derive(Debug, Default)]
    struct Outer {
        asset: Asset,
        name: String,
    }
    structio::object!(Outer { asset, name });

    let json = r#"{"asset":{"version":"2.0","minVersion":1},"name":"n"}"#;
    assert!(from_str::<Outer>(json).is_ok());

    let json = r#"{"asset":{"generator":"g"},"name":"n"}"#;
    let err = from_str::<Outer>(json).unwrap_err();
    assert_eq!(err.code, ErrorCode::MissingKey);
    // The inner brace: what is incomplete is the member, not the document.
    assert_eq!(err.index, json.find(r#"{"generator""#).unwrap());
}

/// Reading into a value that already holds the answer does not excuse the
/// document from carrying it. Absence is a fact about the document, and a
/// destination that happens to be populated cannot make it one about the
/// destination.
#[test]
fn reading_into_a_populated_value_still_requires_the_marks() {
    let mut into = asset();
    assert_eq!(
        structio::read_into(&mut into, r#"{"generator":"h"}"#)
            .unwrap_err()
            .code,
        ErrorCode::MissingKey
    );

    let doc = to_beve(&OnlyGenerator {
        generator: "h".into(),
    });
    let mut into = asset();
    assert_eq!(
        read_beve_into(&mut into, &doc).unwrap_err().code,
        ErrorCode::MissingKey
    );
}

/// A value streamed out of a sequence of documents is read by the ordinary
/// parser, so it meets the marks like any other. Worth pinning because the
/// streaming side frames rather than decides, and a refactor that moved the
/// check could lose it here alone.
#[test]
fn a_streamed_document_meets_the_marks() {
    let input = "{\"version\":\"2.0\",\"minVersion\":1}\n{\"generator\":\"g\"}\n";
    let mut docs = Documents::lines(input.as_bytes());

    let first: Asset = docs.next_value().unwrap().unwrap();
    assert_eq!(first.version, "2.0");
    let err = docs.next_value::<Asset>().unwrap().unwrap_err();
    assert_eq!(err.as_parse().unwrap().code, ErrorCode::MissingKey);
}

/// A pointer read decodes one value out of a BEVE document by walking the
/// headers in front of it, and lands in the same object reader.
#[test]
fn a_pointer_read_meets_the_marks() {
    #[derive(Default)]
    struct Outer {
        good: NoGenerator,
        bad: OnlyGenerator,
    }
    structio::object!(Outer { good, bad });

    let doc = to_beve(&Outer {
        good: NoGenerator {
            version: "2.0".into(),
            min_version: 1,
        },
        bad: OnlyGenerator {
            generator: "g".into(),
        },
    });

    assert_eq!(
        beve::from_slice_at::<Asset>(&doc, "/good").unwrap().version,
        "2.0"
    );
    assert_eq!(
        beve::from_slice_at::<Asset>(&doc, "/bad").unwrap_err().code,
        ErrorCode::MissingKey
    );
}

// ---------------------------------------------------------------------------
// Meeting the policy
// ---------------------------------------------------------------------------

/// The two are a union rather than an override: `RequireKeys` still asks for
/// the member no mark did.
#[test]
fn require_keys_still_requires_the_unmarked_field() {
    assert!(from_str::<Asset>(MINIMAL).is_ok());
    assert_eq!(
        from_str_with::<RequireKeys, Asset>(MINIMAL)
            .unwrap_err()
            .code,
        ErrorCode::MissingKey
    );

    let complete = r#"{"version":"2.0","minVersion":1,"generator":"g"}"#;
    assert_eq!(
        from_str_with::<RequireKeys, Asset>(complete).unwrap(),
        asset()
    );

    let doc = to_beve(&asset());
    assert_eq!(from_beve_with::<RequireKeys, Asset>(&doc).unwrap(), asset());
}

/// A key nothing claims is stepped over rather than counted. It can neither
/// satisfy a mark nor disturb one, whichever way the unknown-key policy goes.
#[test]
fn an_unknown_key_neither_satisfies_a_mark_nor_disturbs_one() {
    let missing = r#"{"zzz":1,"generator":"g"}"#;
    let complete = r#"{"zzz":1,"version":"2.0","minVersion":1}"#;

    // Refused for the unknown key under the default policy, before absence is
    // even looked at.
    assert_eq!(
        from_str::<Asset>(missing).unwrap_err().code,
        ErrorCode::UnknownKey
    );

    assert_eq!(
        from_str_with::<SkipUnknown, Asset>(missing)
            .unwrap_err()
            .code,
        ErrorCode::MissingKey
    );
    assert_eq!(
        from_str_with::<SkipUnknown, Asset>(complete)
            .unwrap()
            .version,
        "2.0"
    );
}

/// Writing under `SkipNull` and reading a marked `Option` contradict each
/// other, exactly as `SkipNull` and `RequireKeys` already do. The writer drops
/// the member and the reader insists on it, so a document that round-trips
/// under one policy does not under the pair. Marking a field whose absence the
/// writer can produce is the mistake; the test is here to say the crate does
/// not paper over it.
#[test]
fn skip_null_drops_a_member_a_mark_then_demands() {
    #[derive(Debug, Default)]
    struct Note {
        text: Option<String>,
    }
    structio::object!(Note {
        #[required]
        text
    });

    let written = to_string_with::<SkipNull, _>(&Note { text: None });
    assert_eq!(written, "{}");
    assert_eq!(
        from_str::<Note>(&written).unwrap_err().code,
        ErrorCode::MissingKey
    );

    // A member that is present and null satisfies the mark: the document said
    // something about the field, and what it said was "nothing".
    assert!(from_str::<Note>(r#"{"text":null}"#).is_ok());
}

// ---------------------------------------------------------------------------
// Grammar
// ---------------------------------------------------------------------------

/// The marker sits in front of every form a field can take, and in front of a
/// declaration for one format alone.
#[test]
fn the_marker_composes_with_the_rest_of_a_declaration() {
    #[derive(Debug, Default)]
    struct Every {
        plain: u32,
        renamed: u32,
        adapted: u32,
        both: u32,
    }
    structio::json_object!(Every {
        #[required]
        plain,
        #[required]
        "wire" => renamed,
        #[required]
        adapted as Same,
        #[required]
        "b" => both as Same,
    });

    assert_eq!(Every::REQUIRED, 0b1111);
    assert!(from_str::<Every>(r#"{"plain":1,"wire":2,"adapted":3,"b":4}"#).is_ok());
    assert_eq!(
        from_str::<Every>(r#"{"plain":1,"wire":2,"adapted":3}"#)
            .unwrap_err()
            .code,
        ErrorCode::MissingKey
    );

    #[derive(Debug, Default)]
    struct BeveOnly {
        a: u32,
        b: u32,
    }
    structio::beve_object!(BeveOnly {
        #[required]
        a,
        b
    });
    assert_eq!(BeveOnly::REQUIRED, 0b01);
}

/// A type that borrows from the input takes the other arm of the declaration
/// macro, the one that carries `'de` through verbatim, so the mask has to reach
/// it by a different route than the generic case below.
#[test]
fn a_borrowing_declaration_may_mark_a_field() {
    #[derive(Debug, Default)]
    struct Frame<'a> {
        id: u32,
        payload: &'a [u8],
    }
    // `beve_object!`, a borrowed `&[u8]` being a thing only BEVE hands back.
    structio::beve_object!(['de] Frame<'de> {
        #[required]
        id,
        payload,
    });

    assert_eq!(<Frame<'_> as Keys>::REQUIRED, 0b01);

    #[derive(Default)]
    struct OnlyPayload {
        payload: Vec<u8>,
    }
    structio::beve_object!(OnlyPayload { payload });

    let doc = to_beve(&OnlyPayload {
        payload: vec![1, 2, 3],
    });
    assert_eq!(
        from_beve::<Frame<'_>>(&doc).unwrap_err().code,
        ErrorCode::MissingKey
    );
}

/// Generics reach the mask the same way they reach the key list: the mark is on
/// the field, and the field is on the type whatever fills its parameter.
#[test]
fn a_generic_declaration_may_mark_a_field() {
    #[derive(Debug)]
    struct Page<T> {
        items: Vec<T>,
        cursor: Option<String>,
    }

    // By hand rather than derived: the derive would ask for `T: Default` on
    // `Page` itself, which neither field needs. The declaration asks for it
    // anyway, a `Vec<T>` being read by building elements.
    impl<T> Default for Page<T> {
        fn default() -> Self {
            Page {
                items: Vec::new(),
                cursor: None,
            }
        }
    }
    structio::object!([T: structio::ReadWrite + Default] Page<T> {
        #[required]
        items,
        cursor,
    });

    assert_eq!(<Page<u32> as Keys>::REQUIRED, 0b01);
    assert!(from_str::<Page<u32>>(r#"{"items":[1,2]}"#).is_ok());
    assert_eq!(
        from_str::<Page<u32>>(r#"{"cursor":"c"}"#).unwrap_err().code,
        ErrorCode::MissingKey
    );
}

// ---------------------------------------------------------------------------
// The 64-field line
// ---------------------------------------------------------------------------

/// `RequireKeys` needs a bit for every field and so refuses a struct of more
/// than 64. A mark needs a bit only for itself, so a wider struct may still
/// have one, as long as what it marks is among the first 64 declared. That
/// asymmetry is the whole of the cap's reach here, and it is worth pinning
/// because the field past the line is the one an implementation would silently
/// get wrong: a shift by 64 wraps to bit 0 on most machines, which would credit
/// the wrong field.
macro_rules! wide {
    ($first:ident, $($mid:ident),*; $last:ident, $($past:ident),* $(,)?) => {
        #[derive(Debug, Default)]
        struct Wide {
            $first: u32,
            $($mid: u32,)*
            $last: u32,
            $($past: u32),*
        }
        // The first field and the last one the mask has room for, so the
        // boundary is pinned from below as well as above: a mask built with
        // `<=` where it wanted `<`, or over a `u32`, would drop the second.
        structio::object!(Wide {
            #[required]
            $first,
            $($mid,)*
            #[required]
            $last,
            $($past),*
        });
    };
}

wide!(
    f0, f1, f2, f3, f4, f5, f6, f7, f8, f9, f10, f11, f12, f13, f14, f15, f16, f17, f18, f19, f20,
    f21, f22, f23, f24, f25, f26, f27, f28, f29, f30, f31, f32, f33, f34, f35, f36, f37, f38, f39,
    f40, f41, f42, f43, f44, f45, f46, f47, f48, f49, f50, f51, f52, f53, f54, f55, f56, f57, f58,
    f59, f60, f61, f62;
    f63, f64, f65, f66, f67, f68, f69,
);

#[test]
fn a_struct_too_wide_for_the_policy_may_still_mark_a_field() {
    assert_eq!(Wide::KEYS.len(), 70);
    assert_eq!(Wide::REQUIRED, 1 | (1 << 63));

    assert_eq!(
        from_str::<Wide>(r#"{"f0":1,"f63":2,"f69":3}"#).unwrap().f69,
        3
    );
    // Each mark alone, so neither can stand in for the other.
    assert_eq!(
        from_str::<Wide>(r#"{"f0":1,"f69":2}"#).unwrap_err().code,
        ErrorCode::MissingKey
    );
    assert_eq!(
        from_str::<Wide>(r#"{"f63":1,"f69":2}"#).unwrap_err().code,
        ErrorCode::MissingKey
    );

    // The field a wrapped shift would have credited. Filling it must not look
    // like filling `f0`.
    assert_eq!(
        from_str::<Wide>(r#"{"f64":1}"#).unwrap_err().code,
        ErrorCode::MissingKey
    );
    let doc = to_beve(&BTreeMap::from([("f64".to_string(), 1u32)]));
    assert_eq!(
        from_beve::<Wide>(&doc).unwrap_err().code,
        ErrorCode::MissingKey
    );

    // And the two marks are satisfiable in BEVE as well as in JSON.
    let doc = to_beve(&BTreeMap::from([
        ("f0".to_string(), 1u32),
        ("f63".to_string(), 2u32),
    ]));
    assert_eq!(from_beve::<Wide>(&doc).unwrap().f63, 2);
}
