//! Numbers this crate cannot convert to, handed over as digits.
//!
//! Two things have to hold for the pair to be worth having: the digits must
//! arrive unrounded, and the token must be the one every other reader would
//! have accepted, so that borrowing a number's text is not a way around the
//! grammar.

use structio::{ErrorCode, Options, from_str, json, to_string};

// ---------------------------------------------------------------------------
// A scalar the crate does not describe
// ---------------------------------------------------------------------------

/// A decimal held as an integer and a scale, so that no digit is rounded.
///
/// Exponents are out of scope here: this is a stand-in for `rust_decimal` and
/// friends, and what it has to demonstrate is that the digits survive a real
/// conversion rather than a copy of the input.
#[derive(Default, Debug, PartialEq, Clone)]
struct Fixed {
    /// Every digit of the literal, point removed.
    mantissa: i128,
    /// How many of those digits fall after the point.
    scale: usize,
}

impl Fixed {
    fn parse(text: &str) -> Result<Self, ErrorCode> {
        let (negative, rest) = match text.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, text),
        };
        if rest.contains(['e', 'E']) {
            return Err(ErrorCode::InvalidNumber);
        }
        let (whole, frac) = rest.split_once('.').unwrap_or((rest, ""));
        let mut mantissa: i128 = 0;
        for b in whole.bytes().chain(frac.bytes()) {
            mantissa = mantissa
                .checked_mul(10)
                .and_then(|v| v.checked_add(i128::from(b - b'0')))
                .ok_or(ErrorCode::NumberOutOfRange)?;
        }
        Ok(Fixed {
            mantissa: if negative { -mantissa } else { mantissa },
            scale: frac.len(),
        })
    }

    fn to_text(&self) -> String {
        let digits = self.mantissa.unsigned_abs().to_string();
        let sign = if self.mantissa < 0 { "-" } else { "" };
        if self.scale == 0 {
            return format!("{sign}{digits}");
        }
        // A value under one needs the zeroes the integer form dropped.
        let padded = format!("{:0>width$}", digits, width = self.scale + 1);
        let point = padded.len() - self.scale;
        format!("{sign}{}.{}", &padded[..point], &padded[point..])
    }
}

impl<'de> json::Read<'de> for Fixed {
    fn read<O: Options>(&mut self, p: &mut json::Parser<'de, O>) -> Result<(), ErrorCode> {
        *self = Fixed::parse(p.read_number_str()?)?;
        Ok(())
    }
}

impl json::Write for Fixed {
    fn write<O: Options>(&self, w: &mut json::Writer<'_, O>) {
        w.write_number_str(&self.to_text());
    }
}

#[derive(Default, Debug, PartialEq)]
struct Ledger {
    balance: Fixed,
    entries: Vec<Fixed>,
}
// JSON alone: BEVE has no untyped number, so a type read this way has to pick
// a binary form of its own rather than inherit one from the token.
structio::json_object!(Ledger { balance, entries });

// ---------------------------------------------------------------------------
// The token
// ---------------------------------------------------------------------------

/// The extent, and the grammar behind it. The grammar is the one `read_f64`
/// holds its input to, because it is the same walk; were it not, borrowing a
/// number's text would be a way to get a token past the parser that no
/// conversion would have accepted.
#[test]
fn the_token_is_the_literal_every_other_reader_would_have_read() {
    // Every shape a JSON reader must accept, including the ones that only
    // matter at the edges of the grammar.
    let valid = [
        "0",
        "-0",
        "1",
        "-1",
        "0.5",
        "-0.5",
        "1.25",
        "1e5",
        "1E5",
        "1e+5",
        "1e-5",
        "-1.5E-300",
        "0e0",
        "123456789012345678901234567890",
        "1.7976931348623157e309",
        "0.000000000000000000000000000001",
    ];
    for text in valid {
        let mut p = json::Parser::new(text);
        assert_eq!(p.read_number_str().unwrap(), text, "reading {text:?}");
        assert_eq!(p.position(), text.len(), "cursor after {text:?}");

        let mut f = json::Parser::new(text);
        f.read_f64()
            .unwrap_or_else(|e| panic!("{text:?} as f64: {e:?}"));
        assert_eq!(p.position(), f.position(), "extent of {text:?}");
    }

    // A number ends where the value ends, not where the document does. What
    // follows is the document's business: a separator is fine here and `0x1`
    // is a trailing-content error, but neither is decided by the token.
    for (doc, token) in [("-12.5e3,", "-12.5e3"), ("0x1", "0"), ("1 ", "1")] {
        let mut p = json::Parser::new(doc);
        assert_eq!(p.read_number_str().unwrap(), token, "reading {doc:?}");
        assert_eq!(p.position(), token.len(), "cursor after {doc:?}");
    }
    assert_eq!(
        from_str::<Ledger>(r#"{"balance":0x1,"entries":[]}"#)
            .unwrap_err()
            .code,
        ErrorCode::ExpectedComma
    );
}

#[test]
fn the_grammar_refuses_what_no_reader_would_take() {
    // Each for a different reason: a leading zero, a point or an exponent with
    // no digits behind it, a sign standing alone, a spelling borrowed from
    // some other language.
    let invalid = [
        "", "-", "+1", "01", "-01", ".5", "1.", "1.e5", "1e", "1e+", "1e-", "Infinity", "NaN",
        "true",
    ];
    for text in invalid {
        assert!(
            json::Parser::new(text).read_number_str().is_err(),
            "accepted {text:?}"
        );
        assert!(
            json::Parser::new(text).read_f64().is_err(),
            "read_f64 accepted {text:?}"
        );
    }
}

/// `skip_value` is what a caller reaches for without this method, and it steps
/// over a number by its alphabet rather than by the grammar. That is fine for
/// something being discarded and wrong for something being read, which is the
/// gap this method closes.
#[test]
fn skipping_is_looser_than_reading() {
    let sloppy = "1e--2.3.4";
    let mut skipper = json::Parser::new(sloppy);
    skipper.skip_value().unwrap();
    assert_eq!(skipper.position(), sloppy.len());

    let mut reader = json::Parser::new(sloppy);
    assert!(reader.read_number_str().is_err());
}

#[test]
fn the_text_points_into_the_document() {
    let doc = String::from("  -1.5e10");
    let mut p = json::Parser::new(&doc);
    p.skip_ws();
    let text = p.read_number_str().unwrap();
    assert_eq!(text, "-1.5e10");
    assert!(std::ptr::eq(text.as_ptr(), doc[2..].as_ptr()));
}

// ---------------------------------------------------------------------------
// End to end
// ---------------------------------------------------------------------------

#[test]
fn digits_survive_a_value_no_float_could_hold() {
    // Twenty-eight significant digits: an `f64` keeps at most seventeen.
    let json = r#"{"balance":-1234567890.1234567890123456789,"entries":[0.001,1000000]}"#;
    let ledger: Ledger = from_str(json).unwrap();

    assert_eq!(
        ledger.balance,
        Fixed {
            mantissa: -12345678901234567890123456789,
            scale: 19,
        }
    );
    // Seventeen of those digits is all an `f64` would have kept, which is
    // what makes the round trip above worth asserting.
    assert_eq!(to_string(&ledger), json);
}

#[test]
fn a_malformed_number_reaches_the_caller_as_an_error() {
    let err = from_str::<Ledger>(r#"{"balance":01,"entries":[]}"#).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidNumber);

    // And so does one the destination type itself refuses: `Fixed` has no
    // exponent, so the digits arriving is not the same as them fitting.
    let err = from_str::<Ledger>(r#"{"balance":1e5,"entries":[]}"#).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidNumber);
}

/// Writing is past the point where an error could be reported, so an invalid
/// literal is a caller bug and is caught where bugs are caught.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "requires a JSON number literal")]
fn writing_a_non_number_is_a_debug_assertion() {
    let mut w = json::Writer::<structio::Standard>::new();
    w.write_number_str("1.2.3");
}
