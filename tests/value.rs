//! `Value`: the tree for a value with no declared type.
//!
//! What matters is that a value agrees with everything else in the crate: a
//! BEVE document read into one holds exactly what `beve_to_json` would have
//! written, a declared type moved through one comes back equal, and the text
//! a value writes is the text a declared type with the same members writes,
//! the one exception being a whole-valued float, which a value writes as
//! `1.0` so that its kind survives the trip.

use std::collections::BTreeMap;

use structio::{
    Complex, ErrorCode, Matrix, MatrixLayout, Number, Value, beve_to_json, from_value,
    from_value_with, to_beve, to_string, to_value, value,
};

#[derive(Default, Debug, PartialEq, Clone)]
struct Server {
    host: String,
    port: u16,
    tags: Vec<String>,
    ratio: f64,
}
structio::object!(Server {
    host,
    port,
    tags,
    ratio
});

fn server() -> Server {
    Server {
        host: "a".into(),
        port: 8080,
        tags: vec!["x".into(), "y".into()],
        ratio: 0.5,
    }
}

#[test]
fn json_round_trip_keeps_kinds_and_sorts_keys() {
    let text = r#"{"z":1,"a":[true,null,-2,1.5,"s"],"m":{"k":{}}}"#;
    let d = Value::from_json(text).unwrap();
    assert_eq!(
        d.to_string(),
        r#"{"a":[true,null,-2,1.5,"s"],"m":{"k":{}},"z":1}"#
    );
    assert!(d["z"].is_u64());
    assert!(d["a"][2].is_i64() && !d["a"][2].is_u64());
    assert!(d["a"][3].is_f64());
    assert_eq!(d["a"][4], "s");
    assert_eq!(d["a"][0], true);
    assert!(d["a"][1].is_null());
    assert!(d["missing"].is_null());
    assert!(d["m"]["k"].as_object().unwrap().is_empty());
}

#[test]
fn numbers_classify_like_their_tokens() {
    let d = Value::from_json("[0,-0,18446744073709551615,-9223372036854775808,1e3,2.0]").unwrap();
    let items = d.as_array().unwrap();
    assert_eq!(items[0].as_u64(), Some(0));
    assert_eq!(items[1].as_i64(), Some(0));
    assert_eq!(items[2].as_u64(), Some(u64::MAX));
    assert_eq!(items[2].as_i64(), None);
    assert_eq!(items[3].as_i64(), Some(i64::MIN));
    assert_eq!(items[4].as_f64(), Some(1000.0));
    assert!(items[4].is_f64());
    assert_eq!(items[5].to_string(), "2.0");
    // An integer past 64 bits is kept as a float rather than refused.
    let big = Value::from_json("18446744073709551616").unwrap();
    assert!(big.is_f64());
    // A float past `f64` is a number the value cannot hold.
    assert_eq!(
        Value::from_json("1e400").unwrap_err().code,
        ErrorCode::NumberOutOfRange
    );
    assert_eq!(Value::from(f64::NAN), Value::Null);
    assert_eq!(Number::from_f64(f64::INFINITY), None);
    assert_eq!(Value::from(-1i32).as_i64(), Some(-1));
    assert_eq!(Value::from(1i32).as_u64(), Some(1));
}

#[test]
fn a_float_keeps_its_kind_through_text() {
    // The crate writes the `f64` 1.0 as `1`; a value writes `1.0`, so what
    // was a float reads back as one and the value compares equal to itself
    // after a round trip.
    let d = value!({"whole": 1.0, "neg": -0.0, "big": 1e300, "int": 1});
    assert_eq!(
        d.to_string(),
        r#"{"big":1E300,"int":1,"neg":-0.0,"whole":1.0}"#
    );
    let back = Value::from_json(&d.to_string()).unwrap();
    assert_eq!(back, d);
    assert!(back["whole"].is_f64() && back["big"].is_f64());
    assert!(back["int"].is_u64());
    assert_eq!(back.to_beve(), d.to_beve());
    assert_eq!(Value::from_beve(&d.to_beve()).unwrap(), d);
    assert_eq!(format!("{}", Number::from_f64(3.0).unwrap()), "3.0");

    // A declared `f64` reaches a value through the crate's own text, so a
    // whole one arrives as the integer its text is.
    #[derive(Default)]
    struct Ratio {
        r: f64,
    }
    structio::object!(Ratio { r });
    let v = to_value(&Ratio { r: 2.0 }).unwrap();
    assert!(v["r"].is_u64());
    assert_eq!(from_value::<Ratio>(&v).unwrap().r, 2.0);
    // And a value's `1.0` reads into a declared `f64` as any float does.
    assert_eq!(from_value::<Ratio>(&value!({"r": 2.0})).unwrap().r, 2.0);
}

#[test]
fn pointer_navigates_and_unescapes() {
    let d = value!({"a": {"b/c": [10, 20], "~t": 1}, "": 0});
    assert_eq!(d.pointer("").unwrap(), &d);
    assert_eq!(d.pointer("/a/b~1c/1").unwrap().as_u64(), Some(20));
    assert_eq!(d.pointer("/a/~0t").unwrap().as_u64(), Some(1));
    assert_eq!(d.pointer("/").unwrap().as_u64(), Some(0));
    assert!(d.pointer("/a/b~1c/2").is_none());
    assert!(d.pointer("/a/x").is_none());
    assert!(d.pointer("a").is_none());

    let mut d = d;
    *d.pointer_mut("/a/b~1c/0").unwrap() = value!(11);
    assert_eq!(d["a"]["b/c"][0].as_u64(), Some(11));
}

#[test]
fn index_mut_builds_a_tree() {
    let mut d = Value::Null;
    d["a"]["b"] = value!(1);
    d["a"]["c"] = value!([1, 2]);
    d["a"]["c"][1] = value!("two");
    assert_eq!(d.to_string(), r#"{"a":{"b":1,"c":[1,"two"]}}"#);
    assert_eq!(
        d.get("a").and_then(|a| a.get("b")).and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(d["a"].take()["b"].as_u64(), Some(1));
    assert!(d["a"].is_null());
}

#[test]
fn doc_macro_shapes() {
    let host = String::from("h");
    let n = 3;
    let d = value!({
        "host": host,
        "n": n,
        "neg": -n,
        "f": 1.5,
        "t": true,
        "nothing": null,
        "list": [1, "a", [], {}, {"k": [null]}],
        "opt": Some(2),
        "none": None::<u8>,
        "expr": n * 2,
    });
    assert_eq!(
        d.to_string(),
        r#"{"expr":6,"f":1.5,"host":"h","list":[1,"a",[],{},{"k":[null]}],"n":3,"neg":-3,"none":null,"nothing":null,"opt":2,"t":true}"#
    );
    assert_eq!(value!([]), Value::Array(Vec::new()));
    assert_eq!(value!({}), Value::Object(BTreeMap::new()));
    assert_eq!(value!(null), Value::Null);
    assert_eq!(value!("s"), Value::String("s".into()));
    assert_eq!(value!([1, 2,]), value!([1, 2]));
    let key = "dyn";
    assert_eq!(value!({ key: 1 }).to_string(), r#"{"dyn":1}"#);
}

#[test]
fn declared_types_move_through_a_document() {
    let s = server();
    let d = to_value(&s).unwrap();
    // A declared type writes members in declaration order and a document
    // writes them sorted, so the two agree as documents, not as text.
    assert_eq!(Value::from_json(&to_string(&s)).unwrap(), d);
    assert_eq!(d["port"].as_u64(), Some(8080));
    assert_eq!(from_value::<Server>(&d).unwrap(), s);

    // The document is read as text is: a key the type does not declare is
    // refused by default and stepped over under `SkipUnknown`.
    let mut extra = d.clone();
    extra["extra"] = value!(1);
    assert!(from_value::<Server>(&extra).is_err());
    assert_eq!(
        from_value_with::<structio::SkipUnknown, Server>(&extra).unwrap(),
        s
    );
}

#[test]
fn document_as_a_field() {
    #[derive(Default, Debug, PartialEq)]
    struct Envelope {
        id: u32,
        body: Value,
        extra: Option<Value>,
    }
    structio::object!(Envelope { id, body, extra });

    let e = Envelope {
        id: 7,
        body: value!({"x": [1, 2.5, "s"], "y": null}),
        extra: None,
    };
    let text = to_string(&e);
    assert_eq!(
        text,
        r#"{"id":7,"body":{"x":[1,2.5,"s"],"y":null},"extra":null}"#
    );
    assert_eq!(structio::from_str::<Envelope>(&text).unwrap(), e);
    let bytes = to_beve(&e);
    assert_eq!(structio::from_beve::<Envelope>(&bytes).unwrap(), e);
}

#[test]
fn beve_round_trip() {
    let d = value!({"a": [1, -2, 1.5, true, null, "s", {"n": {}}], "b": {"c": [[]]}});
    let bytes = d.to_beve();
    assert_eq!(Value::from_beve(&bytes).unwrap(), d);
    assert_eq!(beve_to_json(&bytes).unwrap(), d.to_string());
}

#[test]
fn beve_reads_what_the_transcoder_writes() {
    #[derive(Default)]
    struct Rich {
        floats: Vec<f32>,
        ints: Vec<i16>,
        bools: Vec<bool>,
        strings: Vec<String>,
        by_int: BTreeMap<u32, String>,
        by_neg: BTreeMap<i8, u8>,
        z: Complex<f64>,
        zs: Vec<Complex<i16>>,
        m: Matrix<f32>,
        big: u64,
        neg: i64,
    }
    structio::object!(Rich {
        floats,
        ints,
        bools,
        strings,
        by_int,
        by_neg,
        z,
        zs,
        m,
        big,
        neg
    });

    let rich = Rich {
        floats: vec![1.5, -2.25, 0.25],
        ints: vec![-3, 4],
        bools: vec![true, false, true, true, false, false, false, true, true],
        strings: vec!["a".into(), "".into()],
        by_int: [(3, "x".to_string()), (1, "y".to_string())]
            .into_iter()
            .collect(),
        by_neg: [(-1, 2u8)].into_iter().collect(),
        z: Complex::new(1.5, -2.5),
        zs: vec![Complex::new(1, 2), Complex::new(-3, 4)],
        m: Matrix::new(MatrixLayout::RowMajor, vec![2, 2], vec![1.5, 2.5, 3.5, 4.5]).unwrap(),
        big: u64::MAX,
        neg: -1,
    };
    let bytes = to_beve(&rich);
    let d = Value::from_beve(&bytes).unwrap();
    // Through a value so key order does not count. The fixture has no
    // whole-valued float, which the transcoder's text would turn into an
    // integer.
    let sorted = Value::from_json(&beve_to_json(&bytes).unwrap()).unwrap();
    assert_eq!(sorted, d);
    assert!(d["floats"][2].is_f64());
    assert_eq!(d["floats"][1].as_f64(), Some(-2.25));
    assert!(d["ints"][0].is_i64());
    assert_eq!(d["bools"].as_array().unwrap().len(), 9);
    assert_eq!(d["by_int"]["1"], "y");
    assert_eq!(d["by_neg"]["-1"].as_u64(), Some(2));
    assert_eq!(d["z"], value!([1.5, -2.5]));
    assert_eq!(d["zs"][1], value!([-3, 4]));
    assert_eq!(d["m"]["layout"], "layout_right");
    assert_eq!(d["big"].as_u64(), Some(u64::MAX));
    assert_eq!(d["neg"].as_i64(), Some(-1));
}

#[test]
fn beve_number_reads_and_refuses() {
    let n: Number = structio::from_beve(&to_beve(&-5i32)).unwrap();
    assert_eq!(n.as_i64(), Some(-5));
    let n: Number = structio::from_beve(&to_beve(&2.5f32)).unwrap();
    assert_eq!(n.as_f64(), Some(2.5));
    let err = structio::from_beve::<Number>(&to_beve(&"s")).unwrap_err();
    assert_eq!(err.code, ErrorCode::ExpectedNumber);
    let err = structio::from_beve::<Value>(&to_beve(&u128::MAX)).unwrap_err();
    assert_eq!(err.code, ErrorCode::NumberOutOfRange);
    let err = structio::from_beve::<Value>(&to_beve(&f64::NAN)).unwrap_err();
    assert_eq!(err.code, ErrorCode::NumberOutOfRange);
}

#[test]
fn display_forms() {
    let d = value!({"a": [1, 2]});
    assert_eq!(format!("{d}"), r#"{"a":[1,2]}"#);
    assert_eq!(format!("{d:#}"), d.to_json_pretty());
    assert!(d.to_json_pretty().contains('\n'));
    assert_eq!("[1]".parse::<Value>().unwrap(), value!([1]));
    assert_eq!(
        Value::from_json("{").unwrap_err().code,
        ErrorCode::UnexpectedEnd
    );
    assert_eq!(
        Value::from_json("nope").unwrap_err().code,
        ErrorCode::ExpectedNull
    );
}

#[test]
fn collects_from_iterators() {
    let arr: Value = (1..=3).collect();
    assert_eq!(arr, value!([1, 2, 3]));
    let obj: Value = [("b", 2), ("a", 1)].into_iter().collect();
    assert_eq!(obj.to_string(), r#"{"a":1,"b":2}"#);
}
