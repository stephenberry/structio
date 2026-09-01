//! A declaration is checked against the type it describes.
//!
//! Naming every field is the default, and leaving one out is a build error that
//! names it. `..` at the end of a declaration says the omission is deliberate,
//! and this is what it then means on the wire: the field is not written, not
//! read, and not touched by a read that fills the rest of the struct.
//!
//! The mistake this closes cannot be tested from here, because it does not
//! compile. `object!(Config { host, port })` against the `Config` below is
//! `error[E0063]: missing field `cache` in initializer of `Config``, pointed at
//! the declaration. What is testable is that the escape hatch behaves.

use structio::{
    Same, array, beve_object, from_beve, from_str, json_object, object, to_beve, to_string,
};

#[derive(Debug, Default, PartialEq)]
struct Config {
    host: String,
    port: u16,
    cache: Vec<u8>,
}

object!(Config { host, port, .. });

#[test]
fn an_undeclared_field_is_not_written() {
    let c = Config {
        host: "example".into(),
        port: 8080,
        cache: vec![1, 2, 3],
    };
    assert_eq!(to_string(&c), r#"{"host":"example","port":8080}"#);
}

#[test]
fn an_undeclared_field_is_not_a_key_the_reader_knows() {
    // Not "unknown to the schema and therefore skipped": unknown outright, the
    // same as any name the type never declared.
    let e = from_str::<Config>(r#"{"host":"a","port":1,"cache":[1]}"#).unwrap_err();
    assert_eq!(e.code, structio::ErrorCode::UnknownKey);
}

#[test]
fn an_undeclared_field_keeps_what_it_held() {
    let mut c = Config {
        host: String::new(),
        port: 0,
        cache: vec![7, 7],
    };
    structio::read_into(&mut c, r#"{"host":"a","port":1}"#).unwrap();
    assert_eq!(
        c,
        Config {
            host: "a".into(),
            port: 1,
            cache: vec![7, 7]
        }
    );
}

#[test]
fn the_same_holds_in_beve() {
    let c = Config {
        host: "example".into(),
        port: 8080,
        cache: vec![1, 2, 3],
    };
    let back: Config = from_beve(&to_beve(&c)).unwrap();
    assert_eq!(
        back,
        Config {
            host: "example".into(),
            port: 8080,
            cache: Vec::new()
        }
    );
}

#[derive(Debug, Default, PartialEq)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}

array!(Vec3 [x, y, ..]);

#[test]
fn a_positional_declaration_that_leaves_a_field_out_is_that_much_shorter() {
    let v = Vec3 {
        x: 1.0,
        y: 2.0,
        z: 3.0,
    };
    assert_eq!(to_string(&v), "[1,2]");
    // And the length it accepts is the declared one, not the struct's.
    assert!(from_str::<Vec3>("[1,2,3]").is_err());
    assert_eq!(
        from_str::<Vec3>("[1,2]").unwrap(),
        Vec3 {
            x: 1.0,
            y: 2.0,
            z: 0.0
        }
    );
}

#[derive(Debug, Default, PartialEq)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
    label: String,
}

// An element type sits in front of the list and `..` at the end of it.
array!(Rgb [u8; r, g, b, ..]);

#[test]
fn an_element_type_and_a_deliberate_omission_coexist() {
    let c = Rgb {
        r: 1,
        g: 2,
        b: 3,
        label: "red".into(),
    };
    assert_eq!(to_string(&c), "[1,2,3]");
    // Still a typed BEVE array of three, the label having no part in it.
    let back: Rgb = from_beve(&to_beve(&c)).unwrap();
    assert_eq!(
        back,
        Rgb {
            r: 1,
            g: 2,
            b: 3,
            label: String::new()
        }
    );
}

#[derive(Debug, Default, PartialEq)]
struct Marked {
    first: String,
    second: u32,
    third: Vec<u32>,
    scratch: bool,
}

// Every per-field form in front of a `..`: a marker, a rename, an adapter, and
// a plain field, under a case rule.
json_object!(Marked as "camelCase" {
    #[required] "FIRST" => first,
    second,
    third as Vec<Same>,
    ..
});

#[test]
fn the_omission_marker_sits_behind_every_field_form() {
    let m = Marked {
        first: "a".into(),
        second: 2,
        third: vec![3],
        scratch: true,
    };
    assert_eq!(to_string(&m), r#"{"FIRST":"a","second":2,"third":[3]}"#);
    assert!(
        from_str::<Marked>(r#"{"second":2}"#).is_err(),
        "the mark still binds"
    );
}

#[derive(Debug, Default, PartialEq)]
struct Opaque {
    hidden: u8,
}

// The degenerate case: a type whose fields are all left out.
beve_object!(Opaque { .. });

#[test]
fn a_declaration_may_name_no_fields_at_all() {
    let o = Opaque { hidden: 9 };
    let back: Opaque = from_beve(&to_beve(&o)).unwrap();
    assert_eq!(back, Opaque { hidden: 0 });
}

#[derive(Debug, Default, PartialEq)]
struct Complete {
    a: u8,
    b: u8,
}

// The ordinary form, with the trailing comma that the `..` grammar has to keep
// telling apart from an omission. `rustfmt` would take the comma back off.
#[rustfmt::skip]
object!(Complete { a, b, });

#[test]
fn a_complete_declaration_still_takes_a_trailing_comma() {
    assert_eq!(to_string(&Complete { a: 1, b: 2 }), r#"{"a":1,"b":2}"#);
}
