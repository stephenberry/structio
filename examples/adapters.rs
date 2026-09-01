//! Describing a type you do not own, without wrapping it in one you do.
//!
//! The orphan rule says you cannot implement `json::Read` for a type from
//! another crate. An *adapter* moves the impl onto a type you do own, and the
//! field keeps its own type: `at as Millis` says how to read and write the
//! `Duration` sitting in `at`, and every reader and writer of `Event` still
//! sees a `Duration`.
//!
//! `std::time::Duration` stands in here for the `chrono::DateTime` or
//! `uuid::Uuid` this exists for. Nothing in the mechanism knows the difference:
//! an adapter's target is simply a type the declaration does not describe.
//!
//! Kept as a runnable example so the version in docs/schemas.md cannot drift
//! from an API that still compiles. The region between the markers below *is*
//! that version; `docs_quote_the_example_verbatim` in tests/docs.rs fails if
//! the two stop matching.
//!
//! `cargo run --example adapters`
// docs:begin
use std::time::Duration;

use structio::{ErrorCode, Options, from_str, json, to_string};

/// The adapter. A unit struct is enough: it is never constructed, only named.
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

#[derive(Default, Debug, PartialEq)]
struct Job {
    id: u32,
    // Still a `Duration`, and still an `Option<Duration>` and a
    // `Vec<Duration>`: only the encoding of them moved.
    elapsed: Duration,
    timeout: Option<Duration>,
    retries: Vec<Duration>,
}

structio::json_object!(Job {
    id,
    "elapsed_ms" => elapsed as Millis,
    timeout as Option<Millis>,
    retries as Vec<Millis>,
});
// docs:end

fn main() {
    let job = Job {
        id: 42,
        elapsed: Duration::from_millis(1500),
        timeout: None,
        retries: vec![Duration::from_millis(100), Duration::from_millis(400)],
    };

    let text = to_string(&job);
    println!("{text}");
    assert_eq!(
        text,
        r#"{"id":42,"elapsed_ms":1500,"timeout":null,"retries":[100,400]}"#
    );

    let back: Job = from_str(&text).unwrap();
    assert_eq!(back, job);
}
