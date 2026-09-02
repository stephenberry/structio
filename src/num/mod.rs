//! Number conversion, hand written for throughput.
//!
//! Parsing uses SWAR for integers and Eisel-Lemire for floats; serialization
//! uses a two-digit table for integers and zmij for floats. Nothing here
//! allocates.

pub(crate) mod atof;
pub(crate) mod atoi;
pub(crate) mod dtoa;
pub(crate) mod itoa;
pub(crate) mod table;
pub(crate) mod zmij;

#[cfg(test)]
mod tests {
    use super::atof::parse_float;
    use super::atoi::{parse_i64, parse_u64};
    use super::dtoa::{MAX_FLOAT_BYTES, write_f32, write_f64};

    /// How many rounds a randomized test should draw.
    ///
    /// Miri interprets rather than executes, at hundreds of times the cost per
    /// round, so under it these become samples. What Miri is here to check is
    /// the unaligned loads in the integer parser, and those are reached in the
    /// first handful of rounds either way.
    const fn rounds(n: u32) -> u32 {
        if cfg!(miri) { n / 1000 + 1 } else { n }
    }

    /// Xorshift64, so the fuzz tests draw a fixed sequence without a
    /// dependency and without `rand`'s state carrying between them.
    fn xorshift(seed: u64) -> impl FnMut() -> u64 {
        let mut state = seed;
        move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        }
    }

    fn pf64(s: &str) -> f64 {
        let mut i = 0;
        let v = parse_float::<f64>(s.as_bytes(), &mut i).unwrap();
        assert_eq!(i, s.len(), "did not consume all of {s:?}");
        v
    }

    fn pf32(s: &str) -> f32 {
        let mut i = 0;
        let v = parse_float::<f32>(s.as_bytes(), &mut i).unwrap();
        assert_eq!(i, s.len(), "did not consume all of {s:?}");
        v
    }

    fn wf64(v: f64) -> String {
        let mut b = [0u8; MAX_FLOAT_BYTES];
        let n = write_f64(v, &mut b).unwrap();
        String::from_utf8(b[..n].to_vec()).unwrap()
    }

    fn wf32(v: f32) -> String {
        let mut b = [0u8; MAX_FLOAT_BYTES];
        let n = write_f32(v, &mut b).unwrap();
        String::from_utf8(b[..n].to_vec()).unwrap()
    }

    #[test]
    fn integers() {
        let mut i = 0;
        assert_eq!(parse_u64(b"0", &mut i).unwrap(), 0);
        i = 0;
        assert_eq!(
            parse_u64(b"18446744073709551615", &mut i).unwrap(),
            u64::MAX
        );
        i = 0;
        assert!(parse_u64(b"18446744073709551616", &mut i).is_err());
        i = 0;
        assert!(parse_u64(b"01", &mut i).is_err());
        i = 0;
        assert_eq!(
            parse_i64(b"-9223372036854775808", &mut i).unwrap(),
            i64::MIN
        );
        i = 0;
        assert!(parse_i64(b"-9223372036854775809", &mut i).is_err());
        i = 0;
        assert_eq!(
            parse_u64(b"123456789012345678", &mut i).unwrap(),
            123456789012345678
        );
    }

    /// Every terminator a JSON number can meet, plus a byte whose high bit is
    /// set. `0xB5` is the one that matters: masked to seven bits it spells
    /// `'5'`, so a digit test that looked only at the low seven would read it
    /// as part of the number.
    const TERMINATORS: [&[u8]; 10] = [
        b"", b",", b"}", b"]", b" ", b"\n", b"\"", b":", b"\xB5", b"\xFF",
    ];

    /// The parser folds whole eight-digit words and then walks what is left
    /// over one byte at a time, so its cases are digit counts either side of a
    /// word boundary, each terminator that can end the run, and whether the
    /// buffer has a whole word left to load at all.
    #[test]
    fn integers_of_every_length_against_every_terminator() {
        for len in 1..=20usize {
            // The largest and smallest values of this width, and one in
            // between whose digits are all distinct so a misplaced lane shows
            // up as a wrong answer rather than a coincidence.
            let mut cases = vec![
                "1".to_owned() + &"0".repeat(len - 1),
                "9".repeat(len),
                (1..=len)
                    .map(|k| char::from(b'0' + (k % 10) as u8))
                    .collect(),
            ];
            cases.retain(|s| !s.starts_with('0'));

            for text in cases {
                let expected: Option<u64> = text.parse().ok();
                for term in TERMINATORS {
                    // Once with the terminator alone, so the number sits at
                    // the end of the buffer and no whole word is loadable, and
                    // once with eight bytes of slack behind it, so the
                    // word-at-a-time path is reachable.
                    for pad in [0usize, 8] {
                        let mut buf = text.clone().into_bytes();
                        buf.extend_from_slice(term);
                        buf.extend(std::iter::repeat_n(b' ', pad));

                        let mut i = 0;
                        let got = parse_u64(&buf, &mut i);
                        match expected {
                            Some(v) => {
                                assert_eq!(got.ok(), Some(v), "{text:?} + {term:?} + {pad} spaces");
                                assert_eq!(i, text.len(), "stopped in the wrong place");
                            }
                            None => assert!(got.is_err(), "{text:?} is past u64::MAX"),
                        }
                    }
                }
            }
        }
    }

    /// The same parser against the standard library's, over random values of
    /// every width, each followed by a terminator so the scan has to find the
    /// end rather than run out of buffer.
    #[test]
    fn integers_agree_with_the_standard_library() {
        let mut next = xorshift(0x5EED_1234_ABCD_0001);
        for _ in 0..rounds(200_000) {
            let raw = next();
            // Draw a width as well as a value, so short numbers -- the ones
            // a document is actually full of -- are as well covered as the
            // long ones that reach the overflow check.
            let width = 1 + (next() % 20) as u32;
            let v = raw % 10u64.checked_pow(width).unwrap_or(u64::MAX).max(1);
            let text = v.to_string();

            let mut buf = text.clone().into_bytes();
            buf.extend_from_slice(b",\"rest\":0");

            let mut i = 0;
            assert_eq!(parse_u64(&buf, &mut i).unwrap(), v, "parsing {text:?}");
            assert_eq!(i, text.len());

            // And the signed path, which narrows the same magnitude.
            if v <= i64::MAX as u64 {
                let neg = format!("-{text},");
                let mut i = 0;
                assert_eq!(parse_i64(neg.as_bytes(), &mut i).unwrap(), -(v as i64));
                assert_eq!(i, neg.len() - 1);
            }
        }
    }

    #[test]
    #[allow(clippy::approx_constant)] // 3.14 here is a literal to format, not pi
    fn float_format_matches_glaze() {
        // Captured from Glaze's zmij::to_chars for the same inputs.
        assert_eq!(wf64(5.0), "5");
        assert_eq!(wf64(3.14), "3.14");
        assert_eq!(wf64(0.1), "0.1");
        assert_eq!(wf64(1e300), "1E300");
        assert_eq!(wf64(1e-300), "1E-300");
        assert_eq!(wf64(1e16), "1E16");
        assert_eq!(wf64(1e17), "1E17");
        assert_eq!(wf64(1e-4), "0.0001");
        assert_eq!(wf64(1e-5), "1E-5");
        assert_eq!(wf64(123456789012345680.0), "1.2345678901234568E17");
        assert_eq!(wf64(0.0), "0");
        assert_eq!(wf64(-0.0), "-0");
        assert_eq!(wf64(1.0 / 3.0), "0.3333333333333333");
        assert_eq!(wf64(2.5e-9), "2.5E-9");
        assert_eq!(wf64(100.0), "100");
        assert_eq!(wf64(1e21), "1E21");
        assert_eq!(wf32(5.0), "5");
        assert_eq!(wf32(3.14), "3.14");
        assert_eq!(wf32(1e30), "1E30");
        assert_eq!(wf32(1e-30), "1E-30");

        // `f32` switches to scientific one exponent earlier than `f64`, at
        // `MAX_FIXED = 6`. Nothing above pinned that boundary, so these are the
        // cases that actually hold the constant in place.
        assert_eq!(wf32(1e5), "100000");
        assert_eq!(wf32(1e6), "1000000");
        assert_eq!(wf32(1e7), "1E7");
        assert_eq!(wf32(1e8), "1E8");
        assert_eq!(wf32(1234567.0), "1234567");
        assert_eq!(wf32(12345678.0), "1.2345678E7");
        assert_eq!(wf32(1e-3), "0.001");
        assert_eq!(wf32(1e-4), "0.0001");
        assert_eq!(wf32(1e-5), "1E-5");
        assert_eq!(wf32(0.5), "0.5");
        assert_eq!(wf32(-0.0), "-0");
        assert_eq!(wf32(f32::MAX), "3.4028235E38");
        assert_eq!(wf32(f32::MIN_POSITIVE), "1.1754944E-38");
        assert_eq!(wf32(1e-45), "1E-45");

        // JSON has no spelling for these, so the writer refuses rather than
        // inventing one. Only the `f64` arm was reachable from the fuzz tests.
        let mut b = [0u8; MAX_FLOAT_BYTES];
        assert!(write_f64(f64::NAN, &mut b).is_none());
        assert!(write_f64(f64::INFINITY, &mut b).is_none());
        assert!(write_f64(f64::NEG_INFINITY, &mut b).is_none());
        assert!(write_f32(f32::NAN, &mut b).is_none());
        assert!(write_f32(f32::INFINITY, &mut b).is_none());
        assert!(write_f32(f32::NEG_INFINITY, &mut b).is_none());
    }

    /// Subnormals and exact powers of two each take their own branch out of
    /// the decimal conversion, and random bit patterns reach neither often.
    /// A `f64` subnormal turns up about once in two thousand draws; an exact
    /// power of two needs all 52 mantissa bits clear, so the fuzz tests never
    /// reach the irregular path at all and these are its only coverage outside
    /// the ignored `f32` sweep. Captured from Glaze's `zmij::to_chars`.
    #[test]
    fn float_edge_paths_match_glaze() {
        // Subnormal: no implicit bit, and the significand needs rescaling.
        assert_eq!(wf64(f64::from_bits(1)), "5E-324");
        assert_eq!(wf64(f64::from_bits(2)), "1E-323");
        assert_eq!(wf64(f64::from_bits(3)), "1.5E-323");
        assert_eq!(wf64(f64::from_bits(5)), "2.5E-323");
        assert_eq!(
            wf64(f64::from_bits(0x8_0000_0000_0000)),
            "1.1125369292536007E-308"
        );
        assert_eq!(
            wf64(f64::from_bits(0xf_ffff_ffff_ffff)),
            "2.225073858507201E-308"
        );
        assert_eq!(wf64(f64::from_bits(0x0f_4240)), "4.940656E-318");
        assert_eq!(wf32(f32::from_bits(1)), "1E-45");
        assert_eq!(wf32(f32::from_bits(2)), "3E-45");
        assert_eq!(wf32(f32::from_bits(0x40_0000)), "5.877472E-39");
        assert_eq!(wf32(f32::from_bits(0x7f_ffff)), "1.1754942E-38");
        assert_eq!(wf32(f32::from_bits(0x0f_4240)), "1.401298E-39");

        // Exact power of two: the rounding interval is lopsided, which is the
        // one case the fast path hands off.
        assert_eq!(wf64(1.0), "1");
        assert_eq!(wf64(2.0), "2");
        assert_eq!(wf64(f64::MIN_POSITIVE), "2.2250738585072014E-308");
        assert_eq!(wf64(f64::MAX), "1.7976931348623157E308");
    }

    #[test]
    fn float_parse_matches_std() {
        let cases = [
            "0",
            "-0",
            "1",
            "-1",
            "3.14",
            "0.1",
            "1e300",
            "1e-300",
            "1e308",
            "1e309",
            "1e-400",
            "2.2250738585072011e-308", // the classic Grisu/strtod boundary case
            "2.2250738585072014e-308",
            "1.7976931348623157e308",
            "4.9406564584124654e-324",
            "9007199254740993",
            "123456789012345678901234567890",
            "0.000000000000000000000000000001",
            "1.000000000000000000000000000001",
            "7.8459735791271921e+65",
            "3.7208862073259515e-64",
        ];
        for c in cases {
            let ours = pf64(c);
            let theirs: f64 = c.parse().unwrap();
            assert_eq!(
                ours.to_bits(),
                theirs.to_bits(),
                "f64 {c}: {ours:e} vs {theirs:e}"
            );
            let ours = pf32(c);
            let theirs: f32 = c.parse().unwrap();
            assert_eq!(ours.to_bits(), theirs.to_bits(), "f32 {c}");
        }
    }

    /// Round tripping is necessary but not sufficient: the output must also be
    /// the *shortest* decimal that round trips. The standard library's `{:e}`
    /// is exactly that, for both widths, so its digits are the reference.
    fn std_digits(s: &str) -> (String, i32) {
        let (mantissa, exp) = s.split_once('e').unwrap();
        let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
        let digits = digits.trim_end_matches('0');
        let digits = if digits.is_empty() { "0" } else { digits };
        (digits.to_string(), exp.parse().unwrap())
    }

    fn our_digits(s: &str) -> (String, i32) {
        // Re-read our own output in the same normalized form.
        let (mantissa, exp) = match s.split_once('E') {
            Some((m, e)) => (m, e.parse::<i32>().unwrap()),
            None => (s, 0),
        };
        let neg_point = mantissa.find('.');
        let raw: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
        let int_len = neg_point.map_or(raw.len(), |p| {
            mantissa[..p].chars().filter(|c| c.is_ascii_digit()).count()
        });
        let trimmed = raw.trim_start_matches('0');
        let leading_zeros = raw.len() - trimmed.len();
        let trimmed = trimmed.trim_end_matches('0');
        let trimmed = if trimmed.is_empty() { "0" } else { trimmed };
        // Scientific exponent of the leading significant digit.
        let sci = exp + int_len as i32 - 1 - leading_zeros as i32;
        (trimmed.to_string(), sci)
    }

    #[test]
    fn float_output_is_shortest() {
        let mut next = xorshift(0x5DEE_CE66_D1CE_4005);

        let mut ties = 0u64;
        for _ in 0..rounds(500_000) {
            let bits = next();
            let v = f64::from_bits(bits);
            if v.is_finite() && v != 0.0 {
                ties += compare(
                    &std_digits(&format!("{:e}", v.abs())),
                    &our_digits(&wf64(v.abs())),
                );
            }
            let f = f32::from_bits(bits as u32);
            if f.is_finite() && f != 0.0 {
                ties += compare(
                    &std_digits(&format!("{:e}", f.abs())),
                    &our_digits(&wf32(f.abs())),
                );
            }
        }
        // Ties are genuinely rare; a flood of them would mean the comparison
        // has stopped testing anything.
        assert!(
            ties < 20_000,
            "{ties} tie-breaks is too many to be plausible"
        );
    }

    /// Compare our digits against the standard library's, returning 1 if the
    /// two differ only by a tie-break.
    ///
    /// Both are shortest and both round trip; when the exact value sits
    /// precisely between two decimals of that length, the choice is a
    /// convention. Ryu rounds half to even, Rust's Grisu path does not always,
    /// so the digits can differ by one in the last place. What must always
    /// hold is the length and the exponent: those are what "shortest" means.
    fn compare(want: &(String, i32), got: &(String, i32)) -> u64 {
        assert_eq!(got.1, want.1, "exponent differs: got {got:?} want {want:?}");
        assert_eq!(
            got.0.len(),
            want.0.len(),
            "digit count differs: got {got:?} want {want:?}"
        );
        if got.0 == want.0 {
            return 0;
        }
        let a: u64 = got.0.parse().unwrap();
        let b: u64 = want.0.parse().unwrap();
        assert_eq!(
            a.abs_diff(b),
            1,
            "not a tie-break: got {got:?} want {want:?}"
        );
        // Our tie-break is round-half-to-even.
        assert_eq!(
            a % 2,
            0,
            "tie should have resolved to an even digit: {got:?}"
        );
        1
    }

    /// Where the float write path actually spends its time.
    /// `cargo test --release -- --ignored --nocapture float_write_breakdown`
    #[test]
    #[ignore = "timing, not a correctness check"]
    fn float_write_breakdown() {
        use std::time::Instant;
        let shapes: [(&str, Vec<f64>); 2] = [
            (
                "arbitrary (17 digits)",
                (0..100_000)
                    .map(|i| (i as f64) * 1.000_000_1 - 500_000.123_456_789)
                    .collect(),
            ),
            (
                "exact decimals (n/8)",
                (0..100_000).map(|i| ((i % 1000) as f64) / 8.0).collect(),
            ),
        ];

        for (label, vals) in shapes {
            let t0 = Instant::now();
            let mut acc = 0u64;
            for &v in &vals {
                let d = super::zmij::digits_f64(v.abs() + 1.0);
                acc = acc.wrapping_add(d.len as u64).wrapping_add(d.e10 as u64);
            }
            let digits_ns = t0.elapsed().as_nanos() as f64 / vals.len() as f64;

            let t0 = Instant::now();
            let mut buf = [0u8; MAX_FLOAT_BYTES];
            let mut n = 0usize;
            for &v in &vals {
                n += write_f64(v + 1.0, &mut buf).unwrap();
            }
            let full_ns = t0.elapsed().as_nanos() as f64 / vals.len() as f64;

            println!("  {label}");
            println!("    digits     : {digits_ns:6.2} ns/value");
            println!(
                "    + render   : {full_ns:6.2} ns/value  (render {:5.2})",
                full_ns - digits_ns
            );
            println!("    (checksums {acc} {n})");
        }
    }

    /// Recover `(digits, exp)` such that the value is `digits * 10^exp`, from
    /// the ASCII form the writer produces.
    fn shortest_parts(d: &super::zmij::Digits) -> (u64, i32) {
        let digits = d.buf[d.start..d.start + d.len]
            .iter()
            .fold(0u64, |acc, &c| acc * 10 + u64::from(c - b'0'));
        (digits, d.e10 - (d.len as i32 - 1))
    }

    /// Write `digits * 10^exp` into a stack buffer, for the shortness check.
    fn spell(digits: u64, exp: i32, buf: &mut [u8; 32]) -> usize {
        let mut n = 0;
        let mut tmp = [0u8; 24];
        let mut d = digits;
        let mut k = 0;
        if d == 0 {
            tmp[0] = b'0';
            k = 1;
        }
        while d > 0 {
            tmp[k] = b'0' + (d % 10) as u8;
            d /= 10;
            k += 1;
        }
        for i in (0..k).rev() {
            buf[n] = tmp[i];
            n += 1;
        }
        buf[n] = b'e';
        n += 1;
        let mut e = exp;
        if e < 0 {
            buf[n] = b'-';
            n += 1;
            e = -e;
        }
        let mut t = [0u8; 8];
        let mut m = 0;
        if e == 0 {
            t[0] = b'0';
            m = 1;
        }
        while e > 0 {
            t[m] = b'0' + (e % 10) as u8;
            e /= 10;
            m += 1;
        }
        for i in (0..m).rev() {
            buf[n] = t[i];
            n += 1;
        }
        n
    }

    /// Every `f32` bit pattern.
    ///
    /// Rather than compare against the standard library's formatting, which
    /// allocates and would make this run for hours, verify the two defining
    /// properties directly:
    ///
    /// 1. Our digits parse back to the identical value.
    /// 2. No decimal with one fewer digit does, in either rounding direction,
    ///    so the output is genuinely the shortest.
    ///
    /// Our parser is checked against the standard library separately, which is
    /// what makes it usable as the reference here.
    ///
    /// `cargo test --release -- --ignored float_f32_exhaustive`
    #[test]
    #[ignore = "several minutes: checks all 2^32 bit patterns"]
    fn float_f32_exhaustive() {
        let mut buf = [0u8; 32];
        let mut checked = 0u64;
        for bits in 0u32..=u32::MAX {
            let v = f32::from_bits(bits);
            if !v.is_finite() || v == 0.0 {
                continue;
            }
            let mag = v.abs();
            let (digits, exp10) = shortest_parts(&super::zmij::digits_f32(mag));

            let n = spell(digits, exp10, &mut buf);
            let mut i = 0;
            let back = parse_float::<f32>(&buf[..n], &mut i).unwrap();
            assert_eq!(
                back.to_bits(),
                mag.to_bits(),
                "bits {bits:#010x}: {} did not round trip",
                core::str::from_utf8(&buf[..n]).unwrap()
            );

            // One digit shorter must fail, both ways.
            if digits >= 10 {
                for cand in [digits / 10, digits / 10 + 1] {
                    let n = spell(cand, exp10 + 1, &mut buf);
                    let mut i = 0;
                    let shorter = parse_float::<f32>(&buf[..n], &mut i).unwrap();
                    assert_ne!(
                        shorter.to_bits(),
                        mag.to_bits(),
                        "bits {bits:#010x}: {} also round trips, so our answer was not shortest",
                        core::str::from_utf8(&buf[..n]).unwrap()
                    );
                }
            }
            checked += 1;
        }
        println!("checked {checked} finite non-zero f32 values");
    }

    /// The real proof: random bit patterns, round tripped through our writer
    /// and our parser, compared against the standard library at both ends.
    #[test]
    fn float_roundtrip_fuzz() {
        let mut next = xorshift(0x243F_6A88_85A3_08D3);

        for _ in 0..rounds(300_000) {
            let bits = next();
            let v = f64::from_bits(bits);
            if !v.is_finite() {
                continue;
            }
            let s = wf64(v);
            // Our text must parse back to the identical value, everywhere.
            let via_std: f64 = s.parse().unwrap();
            assert_eq!(via_std.to_bits(), v.to_bits(), "write {v:e} -> {s}");
            let via_us = pf64(&s);
            assert_eq!(via_us.to_bits(), v.to_bits(), "reparse {s}");

            let f = f32::from_bits(bits as u32);
            if f.is_finite() {
                let s = wf32(f);
                let via_std: f32 = s.parse().unwrap();
                assert_eq!(via_std.to_bits(), f.to_bits(), "write f32 {f:e} -> {s}");
                let via_us = pf32(&s);
                assert_eq!(via_us.to_bits(), f.to_bits(), "reparse f32 {s}");
            }
        }
    }

    /// Decimal strings that are not exactly representable exercise the
    /// Eisel-Lemire path and its fallback far harder than round tripped output.
    #[test]
    fn float_parse_fuzz_against_std() {
        let mut next = xorshift(0x1357_9BDF_2468_ACE0);

        for _ in 0..rounds(200_000) {
            let digits = next() % 1_000_000_000_000_000_000;
            let exp = (next() % 120) as i64 - 60;
            let s = format!("{digits}e{exp}");
            let ours = pf64(&s);
            let theirs: f64 = s.parse().unwrap();
            assert_eq!(ours.to_bits(), theirs.to_bits(), "{s}");

            let ours = pf32(&s);
            let theirs: f32 = s.parse().unwrap();
            assert_eq!(ours.to_bits(), theirs.to_bits(), "f32 {s}");
        }
    }
}
