//! Case rules: one word at the top of a declaration standing in for a key on
//! every field.
//!
//! Two things are worth pinning here beyond the obvious round trips. The rule
//! has to reach a *name*, not a snake_case string, because a variant name
//! arrives already capitalized and goes through the same key path a field does.
//! And a converted key has to be the same key everywhere: the JSON prefix, the
//! BEVE encoding and the perfect hash are three separate compile-time
//! constants, and a declaration that converted only some of them would read
//! back what it never wrote.

use structio::{
    ErrorCode, Options, from_beve, from_str, json, to_beve, to_string, transcode::beve_to_json,
};

// ---------------------------------------------------------------------------
// The rule itself
// ---------------------------------------------------------------------------

/// Every shape of name the splitter has an opinion about, in one struct: a
/// plain snake_case pair, a digit boundary, a run of capitals, the keyword
/// escape, the unused marker, and a name that is one word already.
#[derive(Default, Debug, PartialEq)]
struct Names {
    byte_offset: u32,
    vec3_x: u32,
    http_url: u32,
    type_: u32,
    _scratch: u32,
    id: u32,
}
structio::object!(Names as "camelCase" {
    byte_offset,
    vec3_x,
    http_url,
    type_,
    _scratch,
    id,
});

#[test]
fn a_rule_converts_every_key_the_declaration_does_not_spell_out() {
    assert_eq!(
        to_string(&Names::default()),
        r#"{"byteOffset":0,"vec3X":0,"httpUrl":0,"type":0,"scratch":0,"id":0}"#
    );
}

#[test]
fn the_converted_key_is_the_one_that_reads_back() {
    let v: Names =
        from_str(r#"{"byteOffset":1,"vec3X":2,"httpUrl":3,"type":4,"scratch":5,"id":6}"#).unwrap();
    assert_eq!(
        v,
        Names {
            byte_offset: 1,
            vec3_x: 2,
            http_url: 3,
            type_: 4,
            _scratch: 5,
            id: 6,
        }
    );
}

#[test]
fn the_rust_name_is_no_longer_a_key() {
    // The whole point of a rule is that it changes the document, so the
    // unconverted spelling has to be as unknown as any other stray key.
    assert_eq!(
        from_str::<Names>(r#"{"byte_offset":1}"#).unwrap_err().code,
        ErrorCode::UnknownKey
    );
}

/// The same field under each of the eight rules, so the styles are pinned
/// against one another rather than one at a time.
/// Deliberately `$rule:literal` rather than `$rule:tt`: a rule that has been
/// through someone else's macro is the case a token-matching lookup would
/// reject, and wrapping a declaration in a macro is an ordinary thing to do.
macro_rules! styled {
    ($name:ident, $rule:literal) => {
        #[derive(Default)]
        struct $name {
            http_byte_offset: u32,
        }
        structio::json_object!($name as $rule { http_byte_offset });
    };
}
styled!(Lower, "lowercase");
styled!(Upper, "UPPERCASE");
styled!(Pascal, "PascalCase");
styled!(Camel, "camelCase");
styled!(Snake, "snake_case");
styled!(Screaming, "SCREAMING_SNAKE_CASE");
styled!(Kebab, "kebab-case");
styled!(ScreamingKebab, "SCREAMING-KEBAB-CASE");

#[test]
fn every_rule_spells_the_same_name_its_own_way() {
    let keys = [
        to_string(&Lower::default()),
        to_string(&Upper::default()),
        to_string(&Pascal::default()),
        to_string(&Camel::default()),
        to_string(&Snake::default()),
        to_string(&Screaming::default()),
        to_string(&Kebab::default()),
        to_string(&ScreamingKebab::default()),
    ];
    assert_eq!(
        keys,
        [
            r#"{"httpbyteoffset":0}"#,
            r#"{"HTTPBYTEOFFSET":0}"#,
            r#"{"HttpByteOffset":0}"#,
            r#"{"httpByteOffset":0}"#,
            r#"{"http_byte_offset":0}"#,
            r#"{"HTTP_BYTE_OFFSET":0}"#,
            r#"{"http-byte-offset":0}"#,
            r#"{"HTTP-BYTE-OFFSET":0}"#,
        ]
        .map(String::from)
    );
}

/// The two name shapes the rule has a documented answer for that no ordinary
/// schema would produce, kept honest here rather than only in prose.
#[derive(Default, Debug, PartialEq)]
#[allow(non_snake_case)]
struct Awkward {
    /// A capital next to a character the rule has no case for.
    caféBar: u32,
    /// A raw identifier, whose `r#` is what `stringify!` hands the macro and
    /// is dropped before the rule sees the name.
    r#type: u32,
}
structio::json_object!(Awkward as "camelCase" { caféBar, r#type });

#[test]
fn a_capital_beside_a_non_ascii_byte_keeps_its_case() {
    // The alternative would run the two together and lose the `B`.
    assert!(to_string(&Awkward::default()).contains("\"caféBar\""));
}

#[test]
fn a_raw_identifier_loses_its_prefix() {
    // `stringify!(r#type)` is `"r#type"`, and the prefix comes off before the
    // rule: the escape is Rust syntax for a keyword collision, not a name the
    // wire should ever see.
    let json = to_string(&Awkward::default());
    assert!(json.contains("\"type\""), "{json}");
    assert!(!json.contains("r#"), "{json}");
}

// ---------------------------------------------------------------------------
// What a rule does not touch
// ---------------------------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Mixed {
    http_url: String,
    byte_offset: u32,
    schema_version: u32,
}
structio::object!(Mixed as "camelCase" {
    "httpURL" => http_url,
    #[required] byte_offset,
    schema_version,
});

#[test]
fn an_explicit_key_wins_over_the_rule() {
    // The acronym the rule flattens is exactly the case an override exists
    // for, and a rule must not quietly overrule the name a caller wrote.
    let v = Mixed {
        http_url: "x".into(),
        byte_offset: 1,
        schema_version: 2,
    };
    assert_eq!(
        to_string(&v),
        r#"{"httpURL":"x","byteOffset":1,"schemaVersion":2}"#
    );
    assert_eq!(from_str::<Mixed>(&to_string(&v)).unwrap(), v);
}

#[test]
fn a_required_field_is_still_required_under_a_rule() {
    // The marker and the rule occupy different slots of the field syntax, so
    // this is really asking that they still parse together.
    assert_eq!(
        from_str::<Mixed>(r#"{"httpURL":"x","schemaVersion":2}"#)
            .unwrap_err()
            .code,
        ErrorCode::MissingKey
    );
}

/// A rule applies to keys. An adapter names a type, in a slot that also spells
/// itself `as`, and the two have to coexist at one field.
struct Halved;

impl<'de> json::ReadAs<'de, u32> for Halved {
    fn read<O: Options>(value: &mut u32, p: &mut json::Parser<'de, O>) -> Result<(), ErrorCode> {
        let mut half = 0u32;
        json::Read::read(&mut half, p)?;
        *value = half * 2;
        Ok(())
    }
}

impl json::WriteAs<u32> for Halved {
    fn write<O: Options>(value: &u32, w: &mut json::Writer<'_, O>) {
        json::Write::write(&(*value / 2), w);
    }
}

#[derive(Default, Debug, PartialEq)]
struct Adapted {
    tick_count: u32,
}
structio::json_object!(Adapted as "kebab-case" { tick_count as Halved });

#[test]
fn a_field_adapter_and_a_rule_coexist() {
    assert_eq!(
        to_string(&Adapted { tick_count: 10 }),
        r#"{"tick-count":5}"#
    );
    assert_eq!(
        from_str::<Adapted>(r#"{"tick-count":5}"#).unwrap(),
        Adapted { tick_count: 10 }
    );
}

// ---------------------------------------------------------------------------
// Both formats
// ---------------------------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Ruled {
    byte_offset: u32,
    http_url: String,
}
structio::object!(Ruled as "camelCase" { byte_offset, http_url });

/// The same schema with every key written out, which is what the rule is
/// claimed to be shorthand for.
#[derive(Default, Debug, PartialEq)]
struct Spelled {
    byte_offset: u32,
    http_url: String,
}
structio::object!(Spelled {
    "byteOffset" => byte_offset,
    "httpUrl" => http_url,
});

fn ruled() -> Ruled {
    Ruled {
        byte_offset: 7,
        http_url: "https://example.invalid".into(),
    }
}

fn spelled() -> Spelled {
    Spelled {
        byte_offset: 7,
        http_url: "https://example.invalid".into(),
    }
}

#[test]
fn a_rule_is_shorthand_for_the_keys_it_writes_out() {
    // Byte identity in both formats, which is the only way to say that a rule
    // reached the BEVE key encoding and the JSON prefix alike.
    assert_eq!(to_string(&ruled()), to_string(&spelled()));
    assert_eq!(to_beve(&ruled()), to_beve(&spelled()));
}

#[test]
fn the_beve_document_reads_back_through_the_hash() {
    // Reading confirms the hash's candidate against the same constant, so this
    // is what says the key list was converted too and not just the writers.
    assert_eq!(from_beve::<Ruled>(&to_beve(&ruled())).unwrap(), ruled());
}

#[test]
fn a_transcoded_document_carries_the_converted_keys() {
    assert_eq!(
        beve_to_json(&to_beve(&ruled())).unwrap(),
        to_string(&ruled())
    );
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
enum Mode {
    #[default]
    ReadOnly,
    ReadWrite,
    HTTPProxy,
    Off,
}
structio::unit_enum!(Mode as "kebab-case" { ReadOnly, ReadWrite, HTTPProxy, Off });

#[test]
fn a_variant_name_is_split_into_words_too() {
    // The name arrives capitalized rather than snaked, which is the reason the
    // rule is defined over words instead of over underscores.
    assert_eq!(to_string(&Mode::ReadWrite), r#""read-write""#);
    assert_eq!(to_string(&Mode::HTTPProxy), r#""http-proxy""#);
    assert_eq!(to_string(&Mode::Off), r#""off""#);
    assert_eq!(from_str::<Mode>(r#""read-only""#).unwrap(), Mode::ReadOnly);
}

#[test]
fn a_run_of_unit_variants_is_still_a_beve_string_array() {
    // Unit variants write their names into a string array without a per-element
    // header, which is a separate expansion from the one `write` uses.
    let modes = vec![Mode::ReadWrite, Mode::HTTPProxy];
    let doc = to_beve(&modes);
    assert_eq!(from_beve::<Vec<Mode>>(&doc).unwrap(), modes);
    assert_eq!(
        beve_to_json(&doc).unwrap(),
        r#"["read-write","http-proxy"]"#
    );
}

#[derive(Default, Debug, PartialEq)]
enum Event {
    #[default]
    KeyDown,
    MouseMove(u32),
    TextInput(String),
}
structio::tagged_enum!(Event as "camelCase" {
    KeyDown,
    MouseMove(_),
    "text" => TextInput(_),
});

#[test]
fn a_tagged_variant_is_keyed_by_its_converted_name() {
    assert_eq!(to_string(&Event::KeyDown), r#""keyDown""#);
    assert_eq!(to_string(&Event::MouseMove(3)), r#"{"mouseMove":3}"#);
    assert_eq!(
        to_string(&Event::TextInput("hi".into())),
        r#"{"text":"hi"}"#
    );
    for e in [
        Event::KeyDown,
        Event::MouseMove(3),
        Event::TextInput("hi".into()),
    ] {
        let json = to_string(&e);
        assert_eq!(from_str::<Event>(&json).unwrap(), e);
        assert_eq!(from_beve::<Event>(&to_beve(&e)).unwrap(), e);
    }
}

// ---------------------------------------------------------------------------
// Generics
// ---------------------------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Envelope<T> {
    schema_version: u32,
    inner_value: T,
}
structio::object!([T: structio::ReadWrite + Default] Envelope<T> as "camelCase" {
    schema_version,
    inner_value,
});

#[test]
fn a_rule_survives_the_generics_form() {
    let v = Envelope {
        schema_version: 2,
        inner_value: 9u32,
    };
    assert_eq!(to_string(&v), r#"{"schemaVersion":2,"innerValue":9}"#);
    assert_eq!(from_str::<Envelope<u32>>(&to_string(&v)).unwrap(), v);
}

// ---------------------------------------------------------------------------
// Raw identifiers
// ---------------------------------------------------------------------------

/// `r#` is Rust syntax for a name that collides with a keyword, not part of
/// the name. A field written `r#type` is how you spell a `"type"` key, which
/// is what a tagged payload usually wants, so the prefix comes off before the
/// rule rather than being carried onto the wire.
#[derive(Default, Debug, PartialEq)]
struct Raw {
    r#type: u32,
    r#fn: u32,
    plain: u32,
}
structio::object!(Raw {
    r#type,
    r#fn,
    plain,
});

#[test]
fn a_raw_identifier_drops_its_prefix() {
    let v = Raw {
        r#type: 1,
        r#fn: 2,
        plain: 3,
    };
    assert_eq!(to_string(&v), r#"{"type":1,"fn":2,"plain":3}"#);
    assert_eq!(from_str::<Raw>(&to_string(&v)).unwrap(), v);
}

#[test]
fn a_raw_key_reads_back_through_the_hash_and_through_beve() {
    // The JSON prefix, the BEVE encoding and the perfect hash are three
    // separate constants. All three have to have dropped the prefix, or the
    // document would not read back what it wrote.
    let v = Raw {
        r#type: 7,
        r#fn: 8,
        plain: 9,
    };
    assert_eq!(
        from_str::<Raw>(r#"{"type":7,"fn":8,"plain":9}"#).unwrap(),
        v
    );
    assert_eq!(from_beve::<Raw>(&to_beve(&v)).unwrap(), v);
    assert_eq!(
        beve_to_json(&to_beve(&v)).unwrap(),
        r#"{"type":7,"fn":8,"plain":9}"#
    );
}

#[test]
fn the_prefixed_key_is_gone_rather_than_also_accepted() {
    // Not a lenient alias: `r#type` was never a key this declaration has.
    assert_eq!(
        from_str::<Raw>(r#"{"r#type":1,"fn":2,"plain":3}"#)
            .unwrap_err()
            .code,
        ErrorCode::UnknownKey
    );
}

/// A rule respells the name, not the prefix.
#[derive(Default, Debug, PartialEq)]
struct RawCased {
    r#type: u32,
    r#byte_offset: u32,
}
structio::object!(RawCased as "camelCase" {
    r#type,
    r#byte_offset,
});

#[test]
fn a_rule_sees_the_name_without_its_prefix() {
    let v = RawCased {
        r#type: 1,
        r#byte_offset: 2,
    };
    assert_eq!(to_string(&v), r#"{"type":1,"byteOffset":2}"#);
    assert_eq!(from_str::<RawCased>(&to_string(&v)).unwrap(), v);
}

/// An explicit key is a literal the declaration wrote, so it passes through
/// exactly as written even when it looks like a raw identifier.
#[derive(Default, Debug, PartialEq)]
struct RawLiteral {
    r#type: u32,
}
structio::object!(RawLiteral {
    "r#type" => r#type,
});

#[test]
fn an_explicit_key_is_never_unrawed() {
    assert_eq!(to_string(&RawLiteral { r#type: 5 }), r##"{"r#type":5}"##);
}

/// A variant name goes through the same key path a field name does.
#[derive(Default, Debug, PartialEq)]
#[allow(non_camel_case_types)]
enum RawVariant {
    #[default]
    r#type,
    Plain,
}
structio::unit_enum!(RawVariant { r#type, Plain });

#[test]
fn a_raw_variant_name_drops_its_prefix_too() {
    assert_eq!(to_string(&RawVariant::r#type), r#""type""#);
    assert_eq!(
        from_str::<RawVariant>(r#""type""#).unwrap(),
        RawVariant::r#type
    );
    assert_eq!(
        from_beve::<RawVariant>(&to_beve(&RawVariant::r#type)).unwrap(),
        RawVariant::r#type
    );
}
