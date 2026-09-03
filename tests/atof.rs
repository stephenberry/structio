//! The float scanner against the standard library, bit for bit.
//!
//! `str::parse` is the reference because it is correct by construction, and
//! the crate's own fallback tier already defers to it. Every literal is read
//! both on its own, where it ends inside the last word of the document and
//! takes the byte-at-a-time tail, and as the first element of a longer array,
//! where the whole of it is read a word at a time.

use structio::{ErrorCode, from_str};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn digits(&mut self, n: usize, zero_heavy: bool) -> String {
        (0..n)
            .map(|_| {
                if zero_heavy && self.below(3) != 0 {
                    '0'
                } else {
                    (b'0' + self.below(10) as u8) as char
                }
            })
            .collect()
    }

    /// One valid JSON number literal, shaped to reach every path: any digit
    /// count either side of the point, runs of zeros, and every exponent form.
    fn literal(&mut self) -> String {
        let mut s = String::new();
        if self.below(2) == 0 {
            s.push('-');
        }
        let zero_heavy = self.below(4) == 0;
        match self.below(4) {
            0 => s.push('0'),
            _ => {
                let n = 1 + self.below(24) as usize;
                s.push((b'1' + self.below(9) as u8) as char);
                s.push_str(&self.digits(n - 1, zero_heavy));
            }
        }
        if self.below(3) != 0 {
            s.push('.');
            let n = 1 + self.below(24) as usize;
            s.push_str(&self.digits(n, zero_heavy));
        }
        if self.below(3) == 0 {
            s.push(if self.below(2) == 0 { 'e' } else { 'E' });
            match self.below(3) {
                0 => s.push('-'),
                1 => s.push('+'),
                _ => {}
            }
            let e = match self.below(4) {
                0 => self.below(10),
                1 => self.below(40),
                2 => self.below(330),
                _ => self.below(5000),
            };
            s.push_str(&e.to_string());
        }
        s
    }
}

/// A float's exact bit pattern, widened so both widths compare the same way.
trait Bits {
    fn bits(self) -> u64;
}
impl Bits for f32 {
    fn bits(self) -> u64 {
        self.to_bits() as u64
    }
}
impl Bits for f64 {
    fn bits(self) -> u64 {
        self.to_bits()
    }
}

/// Read `text` as `F` on its own and as the head of a longer array, checking
/// both against `want`.
fn check<F>(text: &str, want: F)
where
    F: Bits + Copy + PartialEq + std::fmt::Debug + for<'de> structio::json::Read<'de> + Default,
    Vec<F>: for<'de> structio::json::Read<'de>,
{
    // Compare the bits rather than the values, so `-0` and `0` differ and
    // NaN is never involved.
    let bits = |v: F| v.bits();
    let alone: F = from_str(text).unwrap_or_else(|e| panic!("{text}: {e:?}"));
    assert_eq!(bits(alone), bits(want), "alone: {text}");
    let doc = format!("[{text},0,0,0,0,0,0,0,0,0,0]");
    let in_array: Vec<F> = from_str(&doc).unwrap_or_else(|e| panic!("{doc}: {e:?}"));
    assert_eq!(bits(in_array[0]), bits(want), "in array: {text}");
}

fn check_both(text: &str) {
    check::<f64>(text, text.parse().unwrap());
    check::<f32>(text, text.parse().unwrap());
}

#[test]
fn random_literals_agree_with_the_standard_library() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..200_000 {
        check_both(&rng.literal());
    }
}

#[test]
fn the_edges_agree_with_the_standard_library() {
    for text in [
        "0",
        "-0",
        "0.0",
        "-0.0",
        "0e0",
        "0E-0",
        "1",
        "1.5",
        "12345678",
        "123456789",
        "1234567890123456789",
        "12345678901234567890",
        "123456789012345678901234567890",
        "100000000000000000000000",
        "100000000000000000000001",
        "0.1",
        "0.000000000000000000000000001",
        "0.0000000000000000000000000012345678901234567890",
        "0.00000001234567890123456789",
        "9007199254740992",
        "9007199254740993",
        "9007199254740993.0",
        "17976931348623157e292",
        "1.7976931348623157e308",
        "1.7976931348623159e308",
        "2.2250738585072011e-308",
        "2.2250738585072014e-308",
        "4.9e-324",
        "2.4703282292062327e-324",
        "2.4703282292062328e-324",
        "1e400",
        "-1e400",
        "1e-400",
        "1e99999999",
        "1e-99999999",
        "1E+5",
        "1e+5",
        "1e-5",
        "3.4028235e38",
        "3.4028236e38",
        "1.4e-45",
        "7.0064923216240854e-46",
        "7.0064923216240853e-46",
        "9999999999999999999",
        "99999999999999999999",
        "999999999999999999999999",
        "1234567.50076821906",
        "193981.50076821906",
        "-52334.03038330453",
        "122.625",
        "0.5",
        "9.87654321e-7",
        "1.00000000000000000000000000000000000000001",
        "0.99999999999999999999999999999999999999999",
        "8.98846567431158e307",
        "4503599627370496.5",
        "4503599627370497.5",
        "1.1920928955078125e-07",
        "16777217",
        "16777216.5",
    ] {
        check_both(text);
    }
}

#[test]
fn the_grammar_is_still_held_to() {
    for (text, code) in [
        ("01", ErrorCode::InvalidNumber),
        ("00", ErrorCode::InvalidNumber),
        ("-01", ErrorCode::InvalidNumber),
        ("1.", ErrorCode::InvalidNumber),
        ("1.e5", ErrorCode::InvalidNumber),
        ("1e", ErrorCode::InvalidNumber),
        ("1e+", ErrorCode::InvalidNumber),
        ("1e-", ErrorCode::InvalidNumber),
        (".5", ErrorCode::ExpectedNumber),
        ("-", ErrorCode::ExpectedNumber),
        ("+1", ErrorCode::ExpectedNumber),
        ("-.5", ErrorCode::ExpectedNumber),
    ] {
        let alone = from_str::<f64>(text).unwrap_err();
        assert_eq!(alone.code, code, "alone: {text}");
        let doc = format!("[{text},0,0,0,0,0,0,0,0,0,0]");
        let in_array = from_str::<Vec<f64>>(&doc).unwrap_err();
        assert_eq!(in_array.code, code, "in array: {text}");
    }
}
