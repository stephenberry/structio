//! `#[derive(Structio)]` against the declaration it expands to.
//!
//! Each shape is declared twice, once through the macro and once through the
//! derive, over types with the same fields. The two have to agree on the JSON
//! text, the BEVE bytes, the key list, the required mask, and the error a bad
//! document produces, because the derive is a front end to the macro and
//! nothing more. A difference here is a translation bug.

#![cfg(feature = "derive")]

use std::time::Duration;

use structio::{
    Elements, ErrorCode, Keys, Options, SkipUnknown, Structio, Variants, beve, from_str,
    from_str_with, json, to_string,
};

/// The JSON text, the BEVE bytes and a JSON round trip through each type.
fn same_wire<M, D>(m: &M, d: &D)
where
    M: json::Write + beve::Write + for<'de> json::Read<'de> + Default + PartialEq + std::fmt::Debug,
    D: json::Write + beve::Write + for<'de> json::Read<'de> + Default + PartialEq + std::fmt::Debug,
{
    let text = to_string(m);
    assert_eq!(text, to_string(d));
    assert_eq!(beve::to_vec(m), beve::to_vec(d));
    assert_eq!(&from_str::<M>(&text).unwrap(), m);
    assert_eq!(&from_str::<D>(&text).unwrap(), d);
}

fn same_keys<M: Keys, D: Keys>() {
    assert_eq!(M::KEYS, D::KEYS);
    assert_eq!(M::REQUIRED, D::REQUIRED);
}

fn same_error<M, D>(text: &str, code: ErrorCode)
where
    M: for<'de> json::Read<'de> + Default + std::fmt::Debug,
    D: for<'de> json::Read<'de> + Default + std::fmt::Debug,
{
    assert_eq!(from_str::<M>(text).unwrap_err().code, code);
    assert_eq!(from_str::<D>(text).unwrap_err().code, code);
}

// A plain struct, renamed keys, a case rule, a required member, a skipped one.

#[derive(Default, Debug, PartialEq)]
struct MacroCamera {
    focal_length: f64,
    sensitivity: u32,
    sensor_id: u32,
    cache: Vec<u8>,
}
structio::object!(MacroCamera as "camelCase" {
    #[required] focal_length,
    "iso" => sensitivity,
    "sensorID" => sensor_id,
    ..
});

#[derive(Default, Debug, PartialEq, Structio)]
#[structio(rename_all = "camelCase")]
struct DeriveCamera {
    #[structio(required)]
    focal_length: f64,
    #[structio(rename = "iso")]
    sensitivity: u32,
    #[structio(rename = "sensorID")]
    sensor_id: u32,
    #[structio(skip)]
    cache: Vec<u8>,
}

#[test]
fn an_object_matches_its_declaration() {
    let m = MacroCamera {
        focal_length: 50.0,
        sensitivity: 200,
        sensor_id: 7,
        cache: vec![],
    };
    let d = DeriveCamera {
        focal_length: 50.0,
        sensitivity: 200,
        sensor_id: 7,
        cache: vec![],
    };
    same_wire(&m, &d);
    assert_eq!(
        to_string(&DeriveCamera {
            cache: vec![1],
            ..d
        }),
        to_string(&d)
    );
    same_keys::<MacroCamera, DeriveCamera>();
    assert_eq!(
        to_string(&d),
        r#"{"focalLength":50,"iso":200,"sensorID":7}"#
    );
    same_error::<MacroCamera, DeriveCamera>(r#"{"iso":1}"#, ErrorCode::MissingKey);
    same_error::<MacroCamera, DeriveCamera>(
        r#"{"focalLength":1,"cache":[]}"#,
        ErrorCode::UnknownKey,
    );
    let skipped: DeriveCamera =
        from_str_with::<SkipUnknown, _>(r#"{"focalLength":1,"cache":[1,2]}"#).unwrap();
    assert!(skipped.cache.is_empty());
}

// An adapter on a field, through `with`.

struct Millis;

impl<'de> json::ReadAs<'de, Duration> for Millis {
    fn read<O: Options>(
        value: &mut Duration,
        p: &mut json::Parser<'de, O>,
    ) -> Result<(), ErrorCode> {
        let mut ms = 0u64;
        json::Read::read(&mut ms, p)?;
        *value = Duration::from_millis(ms);
        Ok(())
    }
}

impl json::WriteAs<Duration> for Millis {
    fn write<O: Options>(value: &Duration, w: &mut json::Writer<'_, O>) {
        json::Write::write(&(value.as_millis() as u64), w);
    }
}

impl<'de> beve::ReadAs<'de, Duration> for Millis {
    fn read<O: Options>(
        value: &mut Duration,
        r: &mut beve::Reader<'de, O>,
    ) -> Result<(), ErrorCode> {
        let mut ms = 0u64;
        beve::Read::read(&mut ms, r)?;
        *value = Duration::from_millis(ms);
        Ok(())
    }
}

impl beve::WriteAs<Duration> for Millis {
    fn write<O: Options>(value: &Duration, w: &mut beve::Writer<'_, O>) {
        beve::Write::write(&(value.as_millis() as u64), w);
    }
}

#[derive(Default, Debug, PartialEq)]
struct MacroJob {
    id: u32,
    elapsed: Duration,
    retries: Vec<Duration>,
}
structio::object!(MacroJob { id, "elapsed_ms" => elapsed as Millis, retries as Vec<Millis> });

#[derive(Default, Debug, PartialEq, Structio)]
struct DeriveJob {
    id: u32,
    #[structio(rename = "elapsed_ms", with = "Millis")]
    elapsed: Duration,
    #[structio(with = "Vec<Millis>")]
    retries: Vec<Duration>,
}

#[test]
fn an_adapter_matches_its_declaration() {
    let m = MacroJob {
        id: 1,
        elapsed: Duration::from_millis(90),
        retries: vec![Duration::from_millis(5)],
    };
    let d = DeriveJob {
        id: 1,
        elapsed: Duration::from_millis(90),
        retries: vec![Duration::from_millis(5)],
    };
    same_wire(&m, &d);
    same_keys::<MacroJob, DeriveJob>();
    assert_eq!(to_string(&d), r#"{"id":1,"elapsed_ms":90,"retries":[5]}"#);
}

// Generics: a lifetime, a type parameter with its own bound, a where clause,
// and a const parameter, none of them restated.

#[derive(Default, Debug, PartialEq)]
struct MacroPage<'a, T: Clone, const N: usize> {
    label: &'a str,
    items: Vec<T>,
    slots: Vec<u8>,
}
structio::object!(['a, T: Clone + structio::ReadWrite + Default, const N: usize] MacroPage<'a, T, N> {
    label, items, slots
});

#[derive(Default, Debug, PartialEq, Structio)]
struct DerivePage<'a, T: Clone, const N: usize>
where
    T: PartialEq,
{
    label: &'a str,
    items: Vec<T>,
    slots: Vec<u8>,
}

#[test]
fn generics_are_read_off_the_type() {
    let m = MacroPage::<'_, u32, 2> {
        label: "p",
        items: vec![1, 2],
        slots: vec![3, 4],
    };
    let d = DerivePage::<'_, u32, 2> {
        label: "p",
        items: vec![1, 2],
        slots: vec![3, 4],
    };
    let text = to_string(&m);
    assert_eq!(text, to_string(&d));
    assert_eq!(beve::to_vec(&m), beve::to_vec(&d));
    assert_eq!(from_str::<DerivePage<'_, u32, 2>>(&text).unwrap(), d);
    same_keys::<MacroPage<'_, u32, 2>, DerivePage<'_, u32, 2>>();
}

// A positional struct, with and without an element type, and a skipped field.

#[derive(Default, Debug, PartialEq)]
struct MacroVec3 {
    x: f64,
    y: f64,
    z: f64,
}
structio::array!(MacroVec3 [x, y, z]);

#[derive(Default, Debug, PartialEq, Structio)]
#[structio(array)]
struct DeriveVec3 {
    x: f64,
    y: f64,
    z: f64,
}

#[derive(Default, Debug, PartialEq)]
struct MacroRgb {
    r: u8,
    g: u8,
    b: u8,
    label: String,
}
structio::array!(MacroRgb [u8; r, g, b, ..]);

#[derive(Default, Debug, PartialEq, Structio)]
#[structio(array, element = "u8")]
struct DeriveRgb {
    r: u8,
    g: u8,
    b: u8,
    #[structio(skip)]
    label: String,
}

#[test]
fn an_array_matches_its_declaration() {
    let m = MacroVec3 {
        x: 1.0,
        y: 2.0,
        z: 3.0,
    };
    let d = DeriveVec3 {
        x: 1.0,
        y: 2.0,
        z: 3.0,
    };
    same_wire(&m, &d);
    assert_eq!(<MacroVec3 as Elements>::LEN, <DeriveVec3 as Elements>::LEN);
    assert_eq!(to_string(&d), "[1,2,3]");
    same_error::<MacroVec3, DeriveVec3>("[1,2]", from_str::<MacroVec3>("[1,2]").unwrap_err().code);

    let m = MacroRgb {
        r: 1,
        g: 2,
        b: 3,
        label: "red".into(),
    };
    let d = DeriveRgb {
        r: 1,
        g: 2,
        b: 3,
        label: "red".into(),
    };
    assert_eq!(to_string(&m), to_string(&d));
    assert_eq!(beve::to_vec(&m), beve::to_vec(&d));
    // The element type is what puts the typed-array header on the bytes.
    assert_eq!(beve::to_vec(&d), beve::to_vec(&[1u8, 2, 3]));
}

// A unit enum, a tagged enum with a payload, and an internally tagged enum.

#[derive(Default, Debug, PartialEq, Clone, Copy)]
enum MacroLevel {
    #[default]
    Low,
    High,
}
structio::unit_enum!(MacroLevel as "lowercase" { Low, "HIGH" => High });

#[derive(Default, Debug, PartialEq, Clone, Copy, Structio)]
#[structio(rename_all = "lowercase")]
enum DeriveLevel {
    #[default]
    Low,
    #[structio(rename = "HIGH")]
    High = 10,
}

#[derive(Default, Debug, PartialEq)]
struct Circle {
    radius: f64,
}
structio::object!(Circle { radius });

#[derive(Default, Debug, PartialEq)]
enum MacroShape {
    #[default]
    Empty,
    Circle(Circle),
    Sides(u32),
}
structio::tagged_enum!(MacroShape { Empty, Circle(_), "sides" => Sides(_) });

#[derive(Default, Debug, PartialEq, Structio)]
enum DeriveShape {
    #[default]
    Empty,
    Circle(Circle),
    #[structio(rename = "sides")]
    Sides(u32),
}

#[derive(Default, Debug, PartialEq)]
enum MacroThreshold {
    #[default]
    Auto,
    Fixed(Circle),
}
structio::tagged_enum!(MacroThreshold as "snake_case" tag "kind" { Auto, Fixed(_) });

#[derive(Default, Debug, PartialEq, Structio)]
#[structio(tag = "kind", rename_all = "snake_case")]
enum DeriveThreshold {
    #[default]
    Auto,
    Fixed(Circle),
}

#[test]
fn a_unit_enum_matches_its_declaration() {
    same_wire(&MacroLevel::High, &DeriveLevel::High);
    same_wire(
        &vec![MacroLevel::Low, MacroLevel::High],
        &vec![DeriveLevel::Low, DeriveLevel::High],
    );
    assert_eq!(
        <MacroLevel as Variants>::VARIANTS,
        <DeriveLevel as Variants>::VARIANTS
    );
    assert_eq!(to_string(&DeriveLevel::Low), "\"low\"");
    same_error::<MacroLevel, DeriveLevel>("\"middle\"", ErrorCode::UnknownVariant);
}

#[test]
fn a_tagged_enum_matches_its_declaration() {
    same_wire(
        &MacroShape::Circle(Circle { radius: 2.0 }),
        &DeriveShape::Circle(Circle { radius: 2.0 }),
    );
    same_wire(&MacroShape::Sides(3), &DeriveShape::Sides(3));
    same_wire(&MacroShape::Empty, &DeriveShape::Empty);
    assert_eq!(
        <MacroShape as Variants>::VARIANTS,
        <DeriveShape as Variants>::VARIANTS
    );
    assert_eq!(to_string(&DeriveShape::Sides(3)), r#"{"sides":3}"#);
    same_error::<MacroShape, DeriveShape>("\"sides\"", ErrorCode::ExpectedBrace);
    same_error::<MacroShape, DeriveShape>("\"Sides\"", ErrorCode::UnknownVariant);
}

#[test]
fn an_internally_tagged_enum_matches_its_declaration() {
    same_wire(
        &MacroThreshold::Fixed(Circle { radius: 1.5 }),
        &DeriveThreshold::Fixed(Circle { radius: 1.5 }),
    );
    same_wire(&MacroThreshold::Auto, &DeriveThreshold::Auto);
    assert_eq!(
        to_string(&DeriveThreshold::Fixed(Circle { radius: 1.5 })),
        r#"{"kind":"fixed","radius":1.5}"#
    );
    assert_eq!(
        from_str::<DeriveThreshold>(r#"{"radius":1.5,"kind":"fixed"}"#).unwrap(),
        DeriveThreshold::Fixed(Circle { radius: 1.5 })
    );
    same_error::<MacroThreshold, DeriveThreshold>(r#"{"radius":1}"#, ErrorCode::ExpectedTag);
}

// One format only.

#[derive(Default, Debug, PartialEq, Structio)]
#[structio(json)]
struct JsonOnly {
    #[structio(with = "Millis")]
    at: Duration,
}

#[derive(Default, Debug, PartialEq, Structio)]
#[structio(beve)]
struct BeveOnly<'a> {
    bytes: &'a [u8],
}

#[derive(Default, Debug, PartialEq, Structio)]
#[structio(beve)]
enum BeveLevel {
    #[default]
    Low,
    High,
}

#[test]
fn a_narrowed_derive_generates_one_format() {
    let j = JsonOnly {
        at: Duration::from_millis(3),
    };
    assert_eq!(to_string(&j), r#"{"at":3}"#);
    assert_eq!(from_str::<JsonOnly>(r#"{"at":3}"#).unwrap(), j);

    let bytes = [1u8, 2, 3];
    let b = BeveOnly { bytes: &bytes };
    let wire = beve::to_vec(&b);
    assert_eq!(beve::from_slice::<BeveOnly<'_>>(&wire).unwrap(), b);

    let wire = beve::to_vec(&BeveLevel::High);
    assert_eq!(
        beve::from_slice::<BeveLevel>(&wire).unwrap(),
        BeveLevel::High
    );
}

// The crate under another name.

mod renamed {
    use structio as sio;

    #[derive(Default, Debug, PartialEq, sio::Structio)]
    #[structio(crate = "sio")]
    pub struct Point {
        pub x: i32,
        pub y: i32,
    }
}

#[test]
fn the_crate_can_be_named() {
    let p = renamed::Point { x: 1, y: -2 };
    assert_eq!(to_string(&p), r#"{"x":1,"y":-2}"#);
    assert_eq!(from_str::<renamed::Point>(r#"{"x":1,"y":-2}"#).unwrap(), p);
}

// Nesting: a derived type as a field of a declared one and the reverse.

#[derive(Default, Debug, PartialEq, Structio)]
struct Outer {
    camera: MacroCamera,
    level: DeriveLevel,
    shape: MacroShape,
}

#[derive(Default, Debug, PartialEq)]
struct MacroOuter {
    camera: DeriveCamera,
    level: MacroLevel,
    shape: DeriveShape,
}
structio::object!(MacroOuter {
    camera,
    level,
    shape
});

#[test]
fn derived_and_declared_types_nest_either_way() {
    let d = Outer {
        camera: MacroCamera {
            focal_length: 1.0,
            ..Default::default()
        },
        level: DeriveLevel::High,
        shape: MacroShape::Sides(4),
    };
    let m = MacroOuter {
        camera: DeriveCamera {
            focal_length: 1.0,
            ..Default::default()
        },
        level: MacroLevel::High,
        shape: DeriveShape::Sides(4),
    };
    same_wire(&m, &d);
    same_keys::<MacroOuter, Outer>();
}

// A raw identifier, which is how a field is named after a keyword. The `r#`
// is Rust syntax rather than part of the name, so it is gone from the key on
// both paths -- the derive inherits this from the macro it expands to.

#[derive(Default, Debug, PartialEq)]
struct MacroRaw {
    r#type: u32,
    r#fn: u32,
}
structio::object!(MacroRaw { r#type, r#fn });

#[derive(Default, Debug, PartialEq, Structio)]
struct DeriveRaw {
    r#type: u32,
    r#fn: u32,
}

#[derive(Default, Debug, PartialEq, Structio)]
#[structio(rename_all = "camelCase")]
struct DeriveRawCased {
    r#byte_offset: u32,
}

#[derive(Default, Debug, PartialEq, Structio)]
struct DeriveRawRenamed {
    #[structio(rename = "kind")]
    r#type: u32,
}

#[test]
fn a_raw_identifier_is_keyed_without_its_prefix() {
    let d = DeriveRaw { r#type: 1, r#fn: 2 };
    let m = MacroRaw { r#type: 1, r#fn: 2 };
    same_wire(&m, &d);
    same_keys::<MacroRaw, DeriveRaw>();
    assert_eq!(to_string(&d), r#"{"type":1,"fn":2}"#);
    assert_eq!(DeriveRaw::KEYS, ["type", "fn"]);
}

#[test]
fn a_rule_and_a_rename_both_see_the_unprefixed_name() {
    assert_eq!(
        to_string(&DeriveRawCased { r#byte_offset: 3 }),
        r#"{"byteOffset":3}"#
    );
    assert_eq!(to_string(&DeriveRawRenamed { r#type: 4 }), r#"{"kind":4}"#);
}
