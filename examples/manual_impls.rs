//! Declaring a schema by hand, without the `object!` macro.
//!
//! This is the escape hatch for anything the macro cannot express: computed
//! fields, custom coercions, wire formats that do not map cleanly onto struct
//! members. It is also the code the macro generates, so reading it shows
//! exactly what `object!` does.
//!
//! Kept as a runnable example so the version in docs/schema-declaration.md
//! cannot drift from an API that still compiles. The region between the markers
//! below *is* that version: `docs_quote_the_example_verbatim` in tests/docs.rs
//! fails if the two stop matching, so the claim is checked rather than
//! promised.
//!
//! This is the JSON half. `Keys` is shared by every format, so adding BEVE
//! means the same four impls again against `beve::Reader` and `beve::Writer`
//! and nothing else; see `structio::beve` for their signatures.
//!
//! `cargo run --example manual_impls`
// docs:begin
use structio::json::{Parser, Read, ReadObject, Write, WriteObject, Writer};
use structio::{ErrorCode, KeyMap, Keys, Options};

#[derive(Default)]
struct Person {
    first_name: String,
    age: u32,
    friends: Vec<String>,
}

impl Keys for Person {
    const KEYS: &'static [&'static str] = &["first_name", "age", "friends"];
    const MAP: &'static KeyMap = &KeyMap::build(Self::KEYS);
}

impl<'de> ReadObject<'de> for Person {
    fn read_field<O: Options>(
        &mut self,
        index: usize,
        p: &mut Parser<'de, O>,
    ) -> Result<bool, ErrorCode> {
        // The index came from a hash, so confirm the key before using it.
        if !p.match_key(Self::KEYS[index]) {
            return Ok(false);
        }
        p.colon()?;
        match index {
            0 => Read::read(&mut self.first_name, p)?,
            1 => Read::read(&mut self.age, p)?,
            2 => Read::read(&mut self.friends, p)?,
            _ => return Ok(false),
        }
        Ok(true)
    }
}

impl WriteObject for Person {
    fn write_fields<O: Options>(&self, w: &mut Writer<'_, O>) {
        w.member("\"first_name\":", &self.first_name);
        w.member("\"age\":", &self.age);
        w.member("\"friends\":", &self.friends);
    }
}

impl<'de> Read<'de> for Person {
    fn read<O: Options>(&mut self, p: &mut Parser<'de, O>) -> Result<(), ErrorCode> {
        p.read_object(self)
    }
}
impl Write for Person {
    fn write<O: Options>(&self, w: &mut Writer<'_, O>) {
        w.write_object(self)
    }
}
// docs:end

fn main() {
    let p: Person = structio::from_str(r#"{"age":3,"first_name":"x","friends":["a"]}"#).unwrap();
    assert_eq!(p.age, 3);
    assert_eq!(p.first_name, "x");
    assert_eq!(
        structio::to_string(&p),
        r#"{"first_name":"x","age":3,"friends":["a"]}"#
    );
    println!("manual impls work");
}
