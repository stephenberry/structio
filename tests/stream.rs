//! Streaming reads and writes.
//!
//! The two things worth proving here are that the structural scan survives
//! being cut at an arbitrary byte, and that draining the writer mid-document
//! produces exactly the bytes the in-memory writer would have. Both are
//! checked exhaustively rather than by sampling: the interesting cases are all
//! boundary cases, and there are few enough boundaries to enumerate.

use std::io::{self, Read as _};

use structio::json::Write as _;
use structio::{Documents, ErrorCode, Feed, Mode, StreamError};

#[derive(Default, Debug, PartialEq, Clone)]
struct Rec {
    id: u64,
    tag: String,
    ratio: f64,
    tags: Vec<String>,
    ok: bool,
}
structio::object!(Rec {
    id,
    tag,
    ratio,
    tags,
    ok
});

#[derive(Default, Debug, PartialEq)]
struct Small {
    id: u64,
}
structio::object!(Small { id });

#[derive(Default, Debug, PartialEq)]
struct Borrowed<'a> {
    tag: &'a str,
}
structio::object!(['de] Borrowed<'de> { tag });

/// How long a document to build, in records.
///
/// The sweeps below try every buffer size over a whole document, which makes
/// them quadratic in its length. Miri interprets rather than executes, so
/// under it the document shrinks: "every buffer size" stays literally true,
/// and the drain path it is there to check is the same one either way.
const fn records(n: u64) -> u64 {
    if cfg!(miri) { n / 10 + 1 } else { n }
}

fn sample(i: u64) -> Rec {
    Rec {
        id: i,
        // Escapes, a multi-byte character, and a control character, so the
        // scanner has to get string state right rather than merely count
        // quotes.
        tag: format!("a\"b\\c\u{7}d\u{20ac}{i}"),
        ratio: 1.5 * i as f64,
        tags: vec![String::new(), "x".into(), format!("{i}{i}")],
        ok: i.is_multiple_of(2),
    }
}

// ---------------------------------------------------------------------------
// Writing to a sink
// ---------------------------------------------------------------------------

#[test]
fn sink_output_matches_the_in_memory_writer_at_every_buffer_size() {
    let value: Vec<Rec> = (0..records(6)).map(sample).collect();
    let want = structio::to_string(&value);

    // Every size from "smaller than any token" up past the whole document, so
    // the drain lands between every pair of adjacent bytes at least once.
    for cap in 1..=want.len() + 4 {
        let mut got = Vec::new();
        structio::to_writer_buffered(&value, &mut got, cap).unwrap();
        assert_eq!(
            String::from_utf8(got).unwrap(),
            want,
            "buffer size {cap} changed the output"
        );
    }
}

#[test]
fn sink_handles_the_shapes_that_rewrite_their_last_byte() {
    // Empty containers never write a trailing comma, so they take the other
    // branch of the close; a drain must not disturb either.
    let cases: Vec<Box<dyn Fn() -> String>> = vec![
        Box::new(|| structio::to_string(&Vec::<u64>::new())),
        Box::new(|| structio::to_string(&Small::default())),
        Box::new(|| structio::to_string(&vec![Vec::<u64>::new(), vec![1]])),
        Box::new(|| structio::to_string(&42u64)),
        Box::new(|| structio::to_string("plain")),
    ];
    let values: Vec<Box<dyn Fn(usize) -> Vec<u8>>> = vec![
        Box::new(|c| to_bytes(&Vec::<u64>::new(), c)),
        Box::new(|c| to_bytes(&Small::default(), c)),
        Box::new(|c| to_bytes(&vec![Vec::<u64>::new(), vec![1]], c)),
        Box::new(|c| to_bytes(&42u64, c)),
        Box::new(|c| to_bytes("plain", c)),
    ];
    for (want, got) in cases.iter().zip(&values) {
        let want = want();
        for cap in 1..=want.len() + 2 {
            assert_eq!(String::from_utf8(got(cap)).unwrap(), want, "at size {cap}");
        }
    }
}

fn to_bytes<T: structio::json::Write + ?Sized>(value: &T, cap: usize) -> Vec<u8> {
    let mut out = Vec::new();
    structio::to_writer_buffered(value, &mut out, cap).unwrap();
    out
}

/// A sink that fails after letting `ok_writes` calls through.
struct FailAfter {
    ok_writes: usize,
    written: usize,
}

impl io::Write for FailAfter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.ok_writes == 0 {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "no"));
        }
        self.ok_writes -= 1;
        self.written += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_failing_sink_reports_once_and_stops_buffering() {
    let value: Vec<Rec> = (0..200).map(sample).collect();
    let mut sink = FailAfter {
        ok_writes: 2,
        written: 0,
    };
    let err = structio::to_writer_buffered(&value, &mut sink, 64).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    // The document is far larger than what got through, so serialization
    // continued past the failure without accumulating it.
    assert!(sink.written < 200);
    assert!(structio::to_string(&value).len() > 10_000);
}

#[test]
fn a_sink_writer_reports_only_what_it_still_holds() {
    let value: Vec<Rec> = (0..20).map(sample).collect();
    let want = structio::to_string(&value);
    let mut out = Vec::new();
    {
        let mut w =
            structio::json::Writer::<structio::Standard>::to_sink_with_capacity(&mut out, 16);
        value.write(&mut w);
        // `len` counts the window, not the document: it agrees with
        // `as_bytes`, and the document is far larger than either.
        assert_eq!(w.len(), w.as_bytes().len());
        assert!(w.len() <= 16, "the window held {} bytes", w.len());
        assert!(want.len() > 16);
        w.finish().unwrap();
    }
    assert_eq!(String::from_utf8(out).unwrap(), want);
}

#[test]
fn a_hand_written_object_impl_cannot_forge_a_bad_string() {
    // `WriteObject` is a safe trait, so nothing guarantees `write_fields`
    // wrote the trailing comma the close is about to overwrite. Closing over
    // a byte that turned out to be part of a character would hand out a
    // `String` that is not UTF-8.
    struct Rogue;
    impl structio::Keys for Rogue {
        const KEYS: &'static [&'static str] = &["a"];
        const MAP: &'static structio::KeyMap = &structio::KeyMap::build(Self::KEYS);
    }
    impl structio::json::WriteObject for Rogue {
        fn write_fields<O: structio::Options>(&self, w: &mut structio::json::Writer<'_, O>) {
            // No trailing comma, and a multi-byte character last.
            w.raw("\"a\":\"x\"");
            w.raw("\u{20ac}");
        }
    }
    impl structio::json::Write for Rogue {
        fn write<O: structio::Options>(&self, w: &mut structio::json::Writer<'_, O>) {
            w.write_object(self);
        }
    }

    let s = structio::to_string(&Rogue);
    assert!(
        std::str::from_utf8(s.as_bytes()).is_ok(),
        "produced a String that is not UTF-8: {:?}",
        s.as_bytes()
    );
}

// ---------------------------------------------------------------------------
// Reading a sequence
// ---------------------------------------------------------------------------

/// A reader that hands out at most `chunk` bytes at a time, so the parsing
/// side sees the same short reads a socket would give it.
struct Choppy<'a> {
    data: &'a [u8],
    chunk: usize,
}

impl io::Read for Choppy<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.chunk.min(buf.len()).min(self.data.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        self.data = &self.data[n..];
        Ok(n)
    }
}

fn ndjson(n: u64) -> String {
    (0..n)
        .map(|i| structio::to_string(&sample(i)) + "\n")
        .collect()
}

fn array(n: u64) -> String {
    structio::to_string(&(0..n).map(sample).collect::<Vec<_>>())
}

fn concatenated(n: u64) -> String {
    (0..n)
        .map(|i| structio::to_string(&sample(i)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn collect(mode: Mode, text: &str, chunk: usize) -> Vec<Rec> {
    let src = Choppy {
        data: text.as_bytes(),
        chunk,
    };
    Documents::new(src, mode)
        .read_size(chunk)
        .iter::<Rec>()
        .map(|r| r.unwrap())
        .collect()
}

#[test]
fn every_mode_reads_the_same_values_at_every_read_size() {
    let want: Vec<Rec> = (0..8).map(sample).collect();
    for (mode, text) in [
        (Mode::Lines, ndjson(8)),
        (Mode::Array, array(8)),
        (Mode::Values, concatenated(8)),
    ] {
        // One byte at a time is the worst case: every value is interrupted at
        // every one of its bytes across the run.
        for chunk in [1, 2, 3, 7, 64, 4096] {
            assert_eq!(collect(mode, &text, chunk), want, "{mode:?} chunk {chunk}");
        }
    }
}

#[test]
fn an_empty_stream_yields_nothing() {
    for (mode, text) in [
        (Mode::Lines, ""),
        (Mode::Lines, "\n\n  \n"),
        (Mode::Values, "   "),
        (Mode::Array, "[]"),
        (Mode::Array, "  [ ]  "),
    ] {
        assert_eq!(collect(mode, text, 1), Vec::new(), "{mode:?} {text:?}");
    }
}

#[test]
fn values_mode_reads_a_single_document() {
    let want = sample(3);
    let text = structio::to_string(&want);
    assert_eq!(collect(Mode::Values, &text, 1), vec![want]);
}

#[test]
fn bare_scalars_end_at_input_and_at_whitespace() {
    let src = Choppy {
        data: b"1 2.5e1  \n 3",
        chunk: 1,
    };
    let got: Vec<f64> = Documents::values(src)
        .read_size(1)
        .iter::<f64>()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(got, [1.0, 25.0, 3.0]);
}

#[test]
fn values_can_borrow_from_the_stream_buffer() {
    let text = "{\"tag\":\"one\"}\n{\"tag\":\"two\"}\n";
    let mut docs = Documents::lines(text.as_bytes());

    // Each borrow pins the reader; the next call cannot run until it is done.
    let first: Borrowed = docs.next_value().unwrap().unwrap();
    assert_eq!(first.tag, "one");
    let second: Borrowed = docs.next_value().unwrap().unwrap();
    assert_eq!(second.tag, "two");
    assert!(docs.next_value::<Borrowed>().is_none());
}

#[test]
fn reading_into_a_reused_value_matches_a_fresh_read() {
    let text = ndjson(5);
    let mut docs = Documents::lines(text.as_bytes());
    let mut reused = Rec::default();
    for i in 0..5 {
        docs.next_value_into(&mut reused).unwrap().unwrap();
        assert_eq!(reused, sample(i));
    }
    assert!(docs.next_value_into(&mut reused).is_none());
}

#[test]
fn the_window_stays_bounded_over_a_long_stream() {
    let text = ndjson(400);
    let src = Choppy {
        data: text.as_bytes(),
        chunk: 4096,
    };
    let mut docs = Documents::lines(src).read_size(4096);
    let one = structio::to_string(&sample(0)).len();
    let mut worst = 0;
    let mut count = 0;
    while docs.next_value_into(&mut Rec::default()).is_some() {
        worst = worst.max(docs.buffered());
        count += 1;
    }
    assert_eq!(count, 400);
    // Compaction keeps the live window near a read's worth, not near the
    // 100 KiB the whole stream occupies.
    assert!(
        worst < 4096 * 3 + one,
        "window grew to {worst} over {} bytes",
        text.len()
    );
}

#[test]
fn offsets_are_reported_against_the_whole_stream() {
    let mut text = ndjson(3);
    let bad_at = text.len();
    text.push_str("{\"id\":}\n");
    let mut docs = Documents::lines(text.as_bytes());
    let mut sink = Rec::default();
    for _ in 0..3 {
        docs.next_value_into(&mut sink).unwrap().unwrap();
    }
    let err = docs.next_value_into(&mut sink).unwrap().unwrap_err();
    let json = err.as_parse().expect("a parse failure, not i/o");
    // Points at the `}` where a value was due, in whole-stream coordinates.
    assert_eq!(json.index, bad_at + 6);
    assert_eq!(&text[json.index..json.index + 1], "}");
}

#[test]
fn a_truncated_value_is_an_error_not_a_silent_stop() {
    for (mode, text) in [
        (Mode::Values, "{\"id\":1"),
        (Mode::Array, "[{\"id\":1}"),
        (Mode::Array, "[{\"id\":1},"),
        (Mode::Lines, "{\"id\":1"),
    ] {
        let src = Choppy {
            data: text.as_bytes(),
            chunk: 1,
        };
        let mut docs = Documents::new(src, mode).read_size(1);
        let last = docs.iter::<Small>().last();
        assert!(
            matches!(last, Some(Err(_))),
            "{mode:?} {text:?} ended quietly"
        );
    }
}

#[test]
fn malformed_framing_is_rejected() {
    let cases: &[(Mode, &str, ErrorCode)] = &[
        (Mode::Array, "{\"id\":1}", ErrorCode::ExpectedBracket),
        (
            Mode::Array,
            "[{\"id\":1} {\"id\":2}]",
            ErrorCode::ExpectedComma,
        ),
        (Mode::Array, "[{\"id\":1}] junk", ErrorCode::TrailingContent),
        (Mode::Values, "}", ErrorCode::UnexpectedCharacter),
    ];
    for &(mode, text, want) in cases {
        let src = Choppy {
            data: text.as_bytes(),
            chunk: 1,
        };
        let mut docs = Documents::new(src, mode).read_size(1);
        let err = docs
            .iter::<Small>()
            .find_map(Result::err)
            .unwrap_or_else(|| panic!("{text:?} was accepted"));
        assert_eq!(err.as_parse().unwrap().code, want, "{text:?}");
    }
}

#[test]
fn a_value_past_the_limit_fails_instead_of_growing() {
    let text = array(200);
    let src = Choppy {
        data: text.as_bytes(),
        chunk: 64,
    };
    // Each element is well under 512 bytes, so the whole stream reads.
    let mut docs = Documents::array(src).max_value(512).read_size(64);
    assert_eq!(docs.iter::<Rec>().filter(|r| r.is_ok()).count(), 200);

    // One element larger than the limit stops it.
    let big = format!("[{{\"tag\":\"{}\"}}]", "x".repeat(4096));
    let src = Choppy {
        data: big.as_bytes(),
        chunk: 64,
    };
    let mut docs = Documents::array(src).max_value(512).read_size(64);
    let err = docs.iter::<Rec>().find_map(Result::err).unwrap();
    assert_eq!(
        err.as_parse().unwrap().code,
        ErrorCode::DocumentTooLarge,
        "the limit did not stop it"
    );
}

#[test]
fn multi_byte_characters_may_straddle_a_read() {
    // Every one of these needs two to four bytes, so at a one-byte read size
    // the boundary falls inside characters repeatedly.
    let text = "{\"tag\":\"\u{20ac}\u{1f600}\u{e9}\"}\n";
    let src = Choppy {
        data: text.as_bytes(),
        chunk: 1,
    };
    let mut docs = Documents::lines(src).read_size(1);
    let got: Rec = docs.next_value().unwrap().unwrap();
    assert_eq!(got.tag, "\u{20ac}\u{1f600}\u{e9}");
}

#[test]
fn invalid_utf8_in_the_stream_is_a_located_error() {
    let bytes: &[u8] = b"{\"tag\":\"\xff\"}\n";
    let mut docs = Documents::lines(bytes);
    let err = docs
        .next_value_into(&mut Rec::default())
        .unwrap()
        .unwrap_err();
    assert_eq!(err.as_parse().unwrap().code, ErrorCode::InvalidUtf8);
}

#[test]
fn an_io_failure_is_distinguishable_from_a_parse_failure() {
    struct Broken;
    impl io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::ConnectionReset, "gone"))
        }
    }
    let mut docs = Documents::lines(Broken);
    let err = docs
        .next_value_into(&mut Rec::default())
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, StreamError::Io(_)));
    assert_eq!(err.as_io().unwrap().kind(), io::ErrorKind::ConnectionReset);
    assert!(err.as_parse().is_none());
}

#[test]
fn interrupted_reads_are_retried() {
    struct Flaky<'a> {
        data: &'a [u8],
        interrupt: bool,
    }
    impl io::Read for Flaky<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.interrupt = !self.interrupt;
            if self.interrupt {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "again"));
            }
            let n = 1.min(buf.len()).min(self.data.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }
    let text = ndjson(3);
    let src = Flaky {
        data: text.as_bytes(),
        interrupt: false,
    };
    let mut docs = Documents::lines(src).read_size(1);
    let got: Vec<Rec> = docs.iter::<Rec>().map(|r| r.unwrap()).collect();
    assert_eq!(got, (0..3).map(sample).collect::<Vec<_>>());
}

// ---------------------------------------------------------------------------
// from_reader
// ---------------------------------------------------------------------------

#[test]
fn from_reader_matches_from_str() {
    let want = sample(9);
    let text = structio::to_string(&want);
    let got: Rec = structio::from_reader(text.as_bytes()).unwrap();
    assert_eq!(got, want);

    let short = Choppy {
        data: text.as_bytes(),
        chunk: 3,
    };
    let got: Rec = structio::from_reader(short).unwrap();
    assert_eq!(got, want);
}

#[test]
fn from_reader_reports_a_parse_failure_as_a_parse_error() {
    let err = structio::from_reader::<Small, _>(&b"{\"id\":}"[..]).unwrap_err();
    assert!(err.as_parse().is_some());
}

// ---------------------------------------------------------------------------
// Feed
// ---------------------------------------------------------------------------

/// Push `text` one byte at a time, taking whatever completes after each byte.
fn feed_bytewise(mode: Mode, text: &str) -> Vec<Rec> {
    let mut feed = Feed::new(mode);
    let mut out = Vec::new();
    for b in text.as_bytes() {
        feed.push(&[*b]);
        while let Some(r) = feed.next_value::<Rec>() {
            out.push(r.unwrap());
        }
    }
    feed.end();
    while let Some(r) = feed.next_value::<Rec>() {
        out.push(r.unwrap());
    }
    out
}

#[test]
fn feeding_one_byte_at_a_time_finds_every_value() {
    let want: Vec<Rec> = (0..6).map(sample).collect();
    assert_eq!(feed_bytewise(Mode::Lines, &ndjson(6)), want);
    assert_eq!(feed_bytewise(Mode::Array, &array(6)), want);
    assert_eq!(feed_bytewise(Mode::Values, &concatenated(6)), want);
}

#[test]
fn every_two_way_split_of_a_document_reads_the_same() {
    let want = sample(4);
    let text = structio::to_string(&want);
    for at in 0..text.len() {
        let mut feed = Feed::values();
        feed.push(&text.as_bytes()[..at]);
        // The value ends at the closing brace, so no proper prefix of it can
        // complete. Asking is also what forces the scan to suspend here.
        assert!(
            feed.next_value::<Rec>().is_none(),
            "value appeared early at split {at}"
        );
        feed.push(&text.as_bytes()[at..]);
        assert_eq!(
            feed.next_value::<Rec>().unwrap().unwrap(),
            want,
            "split at {at}"
        );
    }
}

#[test]
fn a_trailing_scalar_needs_end_to_complete() {
    let mut feed = Feed::values();
    feed.push(b"42");
    assert!(
        feed.next_value::<u64>().is_none(),
        "42 might still become 421"
    );
    feed.end();
    assert_eq!(feed.next_value::<u64>().unwrap().unwrap(), 42);
    // After `end`, `None` means finished rather than "not yet".
    assert!(feed.next_value::<u64>().is_none());
    assert_eq!(feed.buffered(), 0);
}

#[test]
fn end_turns_a_half_written_value_into_an_error() {
    let mut feed = Feed::values();
    feed.push(b"{\"id\":1");
    assert!(feed.next_value::<Small>().is_none());
    feed.end();
    let err = feed.next_value::<Small>().unwrap().unwrap_err();
    assert_eq!(err.as_parse().unwrap().code, ErrorCode::UnexpectedEnd);
}

#[test]
fn feed_reports_what_it_is_holding() {
    let mut feed = Feed::values();
    assert_eq!(feed.buffered(), 0);
    assert_eq!(feed.offset(), 0);
    feed.push(b"{\"id\":1}{\"id\":2");
    assert_eq!(
        feed.next_value::<Small>().unwrap().unwrap(),
        Small { id: 1 }
    );
    assert!(feed.next_value::<Small>().is_none());
    assert_eq!(feed.offset(), 8);
    assert_eq!(feed.buffered(), 7);
}

#[test]
fn a_feed_value_past_the_limit_fails_instead_of_growing() {
    let mut feed = Feed::values().max_value(64);
    feed.push(format!("{{\"tag\":\"{}\"}}", "x".repeat(4096)).as_bytes());
    let err = feed.next_value::<Rec>().unwrap().unwrap_err();
    assert_eq!(err.as_parse().unwrap().code, ErrorCode::DocumentTooLarge);
}

#[test]
fn a_feed_value_can_borrow_until_the_next_call() {
    let mut feed = Feed::values();
    feed.push(b"{\"tag\":\"borrowed\"}");
    let value: Borrowed = feed.next_value().unwrap().unwrap();
    assert_eq!(value.tag, "borrowed");
}

// ---------------------------------------------------------------------------
// Streamed and batch agree
// ---------------------------------------------------------------------------

#[test]
fn a_streamed_read_accepts_exactly_what_from_str_accepts() {
    // Includes both well-formed and malformed documents; the two paths must
    // agree on which is which, since the streaming side only frames and the
    // parser decides.
    let cases = [
        "{\"id\":1}",
        "{\"id\":01}",
        "{\"id\":1,}",
        "{\"id\":+1}",
        "{\"id\": 1 }",
        "{}",
        "{\"id\":1e3}",
        "{\"nope\":[1,2,{\"x\":\"}\"}]}",
        "{\"id\":\"\\u0041\"}",
        "{\"id\":\"\\ud800\"}",
    ];
    for text in cases {
        let batch = structio::from_str::<Small>(text).map_err(|e| e.code);
        let mut feed = Feed::values();
        feed.push(text.as_bytes());
        feed.end();
        let streamed = match feed.next_value::<Small>() {
            Some(Ok(v)) => Ok(v),
            Some(Err(e)) => Err(e.as_parse().unwrap().code),
            None => panic!("{text:?} produced nothing"),
        };
        assert_eq!(batch, streamed, "disagreed about {text:?}");
    }
}

#[test]
fn a_round_trip_through_both_streaming_halves_is_exact() {
    let values: Vec<Rec> = (0..50).map(sample).collect();

    let mut wire = Vec::new();
    for v in &values {
        structio::to_writer_buffered(v, &mut wire, 32).unwrap();
        wire.push(b'\n');
    }

    let src = Choppy {
        data: &wire,
        chunk: 5,
    };
    let mut docs = Documents::lines(src).read_size(5);
    let got: Vec<Rec> = docs.iter::<Rec>().map(|r| r.unwrap()).collect();
    assert_eq!(got, values);
}

#[test]
fn into_parts_reconstructs_the_rest_of_the_stream() {
    let text = "{\"id\":1}\n{\"id\":2}\n{\"id\":3}\n";
    let mut docs = Documents::lines(text.as_bytes());
    assert_eq!(
        docs.next_value::<Small>().unwrap().unwrap(),
        Small { id: 1 }
    );

    // The window pulled the whole input ahead, so `into_inner` alone would
    // lose records 2 and 3. Paired with the unread bytes, nothing is lost.
    let (reader, unread) = docs.into_parts();
    let mut rest = unread;
    io::Read::read_to_end(&mut { reader }, &mut rest).unwrap();
    assert_eq!(rest, b"{\"id\":2}\n{\"id\":3}\n");

    // And those bytes are themselves a well-formed continuation.
    let mut resumed = Documents::lines(&rest[..]);
    let got: Vec<Small> = resumed.iter::<Small>().map(|r| r.unwrap()).collect();
    assert_eq!(got, vec![Small { id: 2 }, Small { id: 3 }]);
}

#[test]
fn into_parts_yields_nothing_unread_once_framing_has_failed() {
    // Framing failure means the position in the stream is no longer known,
    // so there is nothing honest to hand back.
    let mut docs = Documents::array(&b"[{\"id\":1} {\"id\":2}]"[..]);
    assert_eq!(
        docs.next_value::<Small>().unwrap().unwrap(),
        Small { id: 1 }
    );
    assert!(docs.next_value::<Small>().unwrap().is_err());
    let (_, unread) = docs.into_parts();
    assert!(
        unread.is_empty(),
        "handed back bytes with no known position"
    );
}

#[test]
fn documents_hands_back_its_reader() {
    let text = "{\"id\":1}\n{\"id\":2}\n";
    let mut docs = Documents::lines(text.as_bytes());
    assert_eq!(
        docs.next_value::<Small>().unwrap().unwrap(),
        Small { id: 1 }
    );
    // Whatever the window pulled ahead stays in the window, so the reader is
    // returned positioned past it, not rewound.
    let mut rest = String::new();
    docs.into_inner().read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "");
}

#[test]
fn a_failure_ends_the_stream_rather_than_repeating() {
    // A line that will not parse costs that line and nothing else: the
    // newline framing is still intact, so reading carries on.
    let text = "{\"id\":1}\n{\"id\":}\n{\"id\":3}\n";
    let mut docs = Documents::lines(text.as_bytes());
    let mut seen = 0;
    let mut errors = 0;
    let mut scratch = Small::default();
    while let Some(result) = docs.next_value_into(&mut scratch) {
        match result {
            Ok(()) => seen += 1,
            Err(_) => errors += 1,
        }
        assert!(seen + errors <= 8, "the loop did not terminate");
    }
    assert_eq!((seen, errors), (2, 1));

    // A framing failure is the one that ends things. The documented loop
    // shape is `while let Some(r) = ...`, so a caller who logs the error and
    // carries on must not spin on the same bad bytes.
    let mut feed = Feed::values();
    feed.push(b"} nonsense");
    let mut count = 0;
    while let Some(result) = feed.next_value::<Small>() {
        assert!(result.is_err());
        count += 1;
        assert!(count <= 4, "the loop did not terminate");
    }
    assert_eq!(count, 1);
}

/// A sink that records each write separately, so the cut points can be checked.
#[derive(Default)]
struct Pieces(Vec<Vec<u8>>);

impl io::Write for Pieces {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.push(buf.to_vec());
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn draining_never_cuts_a_character_in_half() {
    // A drain removes a prefix of the buffer, and `into_string` converts what
    // is left without revalidating. If a cut landed inside a character, the
    // retained tail would not be UTF-8 and that conversion would be unsound.
    #[derive(Default)]
    struct Text {
        a: String,
        b: String,
    }
    structio::object!(Text { a, b });

    let value = Text {
        // Two, three, and four byte characters, back to back and adjacent to
        // the ASCII the writer emits around them.
        a: "\u{e9}\u{20ac}\u{1f600}\u{e9}\u{e9}\u{20ac}".into(),
        b: "\u{1f600}\u{1f600}x\u{20ac}".into(),
    };
    let want = structio::to_string(&value);

    for cap in 1..=want.len() + 2 {
        let mut sink = Pieces::default();
        {
            let mut w =
                structio::json::Writer::<structio::Standard>::to_sink_with_capacity(&mut sink, cap);
            value.write(&mut w);
            // Every drain must leave a tail that is valid UTF-8 on its own.
            assert!(
                std::str::from_utf8(w.as_bytes()).is_ok(),
                "buffer tail split a character at size {cap}"
            );
            w.finish().unwrap();
        }
        let mut joined = Vec::new();
        for piece in &sink.0 {
            assert!(
                std::str::from_utf8(piece).is_ok(),
                "a write to the sink split a character at size {cap}"
            );
            joined.extend_from_slice(piece);
        }
        assert_eq!(String::from_utf8(joined).unwrap(), want, "at size {cap}");
    }
}

// ---------------------------------------------------------------------------
// Framing agrees with the batch parser
// ---------------------------------------------------------------------------

#[test]
fn array_framing_rejects_what_the_parser_rejects() {
    // The splitter frames; the parser decides. Where the splitter has to make
    // a grammar decision of its own -- which positions may hold `]` -- it must
    // reach the same verdict `from_str` does.
    let cases = [
        "[]",
        "[ ]",
        "[1]",
        "[1,2]",
        "[1, 2 , 3]",
        "[[1],[2]]",
        "[{\"id\":1}]",
        "[1,]",
        "[1,2,]",
        "[1 , ]",
        "[,]",
        "[,1]",
        "[1,,2]",
        "[1 2]",
        "[1",
        "[1,",
        "[tru",
        "]",
        "[",
        "[]]",
        "[] 1",
    ];
    for text in cases {
        let batch = structio::from_str::<Vec<u64>>(text).is_ok();
        let src = Choppy {
            data: text.as_bytes(),
            chunk: 1,
        };
        let mut docs = Documents::array(src).read_size(1);
        let streamed = docs.iter::<u64>().all(|r| r.is_ok());
        assert_eq!(batch, streamed, "disagreed about {text:?}");
    }
}

#[test]
fn a_dead_feed_stops_accepting_bytes() {
    // The limit is what stops a hostile producer from exhausting memory, so it
    // must survive the failure that producer is most likely to cause.
    let mut feed = Feed::values().max_value(64);
    feed.push(b"}");
    assert!(feed.next_value::<Small>().unwrap().is_err());
    for _ in 0..200 {
        feed.push(&[b'x'; 1024]);
        assert!(feed.next_value::<Small>().is_none());
    }
    assert_eq!(feed.buffered(), 0, "a dead feed went on buffering");
}

#[test]
fn the_read_size_does_not_widen_the_limit() {
    // The limit has to bound the window, not the window plus one read, or the
    // 64 KiB default read size would make any smaller limit meaningless. The
    // reader records how much it was ever asked for, which is the thing that
    // decides how far past the limit the window can get.
    struct Recording<'a> {
        data: &'a [u8],
        asked: std::cell::Cell<usize>,
    }
    impl io::Read for Recording<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.asked.set(self.asked.get().max(buf.len()));
            let n = buf.len().min(self.data.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    let big = format!("[\"{}\"]", "x".repeat(1 << 16));
    let src = Recording {
        data: big.as_bytes(),
        asked: std::cell::Cell::new(0),
    };
    let mut docs = Documents::array(src).max_value(256).read_size(1 << 16);
    let failed = docs.iter::<Rec>().any(|r| r.is_err());
    assert!(failed, "the limit did not stop it");
    let asked = docs.into_inner().asked.get();
    assert!(
        asked <= 257,
        "a single read asked for {asked} bytes against a limit of 256"
    );

    // Without a limit the full read size is still used.
    let src = Recording {
        data: big.as_bytes(),
        asked: std::cell::Cell::new(0),
    };
    let mut docs = Documents::array(src).read_size(4096);
    let _ = docs.iter::<Rec>().count();
    assert_eq!(docs.into_inner().asked.get(), 4096);
}

#[test]
fn the_too_large_error_points_at_the_start_of_the_value() {
    // Whether the value was complete on arrival or still growing, the offset
    // that means something is where it began.
    for text in ["{\"id\":123456789}", "{\"id\":1234567"] {
        let mut feed = Feed::values().max_value(4);
        feed.push(text.as_bytes());
        let err = feed.next_value::<Small>().unwrap().unwrap_err();
        let json = err.as_parse().unwrap();
        assert_eq!(json.code, ErrorCode::DocumentTooLarge, "{text:?}");
        assert_eq!(json.index, 0, "{text:?}");
    }
}

#[test]
fn a_borrowed_value_survives_the_window_moving_under_it() {
    // Compaction shifts the buffer down and rebases every offset the splitter
    // holds. A small read size forces it hundreds of times.
    let n = records(400);
    let text: String = (0..n)
        .map(|i| format!("{{\"tag\":\"tag {i}\"}}\n"))
        .collect();
    let src = Choppy {
        data: text.as_bytes(),
        chunk: 3,
    };
    let mut docs = Documents::lines(src).read_size(3);
    let mut i = 0;
    while let Some(result) = docs.next_value::<Borrowed>() {
        assert_eq!(result.unwrap().tag, format!("tag {i}"));
        i += 1;
    }
    assert_eq!(i, n);
}

#[test]
fn ndjson_accepts_crlf_and_a_missing_final_newline() {
    let text = "{\"id\":1}\r\n{\"id\":2}\r\n{\"id\":3}";
    for chunk in [1, 3, 4096] {
        let src = Choppy {
            data: text.as_bytes(),
            chunk,
        };
        let mut docs = Documents::lines(src).read_size(chunk);
        let ids: Vec<u64> = docs.iter::<Small>().map(|r| r.unwrap().id).collect();
        assert_eq!(ids, [1, 2, 3], "chunk {chunk}");
    }

    // The same, pushed: a final line with no newline needs `end` to complete.
    let mut feed = Feed::lines();
    feed.push(b"{\"id\":1}");
    assert!(feed.next_value::<Small>().is_none());
    feed.end();
    assert_eq!(
        feed.next_value::<Small>().unwrap().unwrap(),
        Small { id: 1 }
    );
}

#[test]
fn ndjson_holds_one_value_to_a_line() {
    // This is what separates `Lines` from `Values`, and it is the reason a
    // corrupt record costs one line rather than the rest of the stream.
    let mut docs = Documents::lines(&b"{\"id\":1} {\"id\":2}\n{\"id\":3}\n"[..]);
    let got: Vec<_> = docs.iter::<Small>().collect();
    assert_eq!(got.len(), 2);
    assert_eq!(
        got[0].as_ref().unwrap_err().as_parse().unwrap().code,
        ErrorCode::TrailingContent
    );
    assert_eq!(*got[1].as_ref().unwrap(), Small { id: 3 });
}

#[test]
fn the_limit_applies_to_newline_framing_too() {
    let text = format!("{{\"id\":1}}\n{{\"tag\":\"{}\"}}\n", "x".repeat(4096));
    let src = Choppy {
        data: text.as_bytes(),
        chunk: 64,
    };
    let mut docs = Documents::lines(src).max_value(512).read_size(64);
    let got: Vec<_> = docs.iter::<Rec>().collect();
    assert!(got[0].is_ok());
    assert_eq!(
        got[1].as_ref().unwrap_err().as_parse().unwrap().code,
        ErrorCode::DocumentTooLarge
    );
}

#[test]
fn an_empty_feed_ends_cleanly_in_every_mode() {
    for mode in [Mode::Values, Mode::Lines, Mode::Array] {
        let mut feed = Feed::new(mode);
        feed.end();
        assert!(feed.next_value::<Small>().is_none(), "{mode:?}");
    }
}

#[test]
fn a_reader_that_stops_early_stops_the_stream() {
    // `Ok(0)` is end of input per the `io::Read` contract, even if the reader
    // would have produced more had it been asked again. Pinning it because it
    // is silent.
    struct StopsOnce<'a> {
        data: &'a [u8],
        stopped: bool,
    }
    impl io::Read for StopsOnce<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.stopped {
                self.stopped = true;
                return Ok(0);
            }
            let n = buf.len().min(self.data.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }
    let mut docs = Documents::lines(StopsOnce {
        data: b"{\"id\":1}\n",
        stopped: false,
    });
    assert!(docs.iter::<Small>().next().is_none());
}

/// The drop guard only exists in debug builds, which is where a test that
/// exercises a `debug_assert` belongs.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "dropped without `finish`")]
fn dropping_a_sink_writer_unfinished_is_caught() {
    // Forgetting `finish` truncates the output and reports no error for it.
    // Nothing in the type system prevents that, so it is at least loud.
    let mut out = Vec::new();
    let mut w = structio::json::Writer::<structio::Standard>::to_sink_with_capacity(&mut out, 4);
    let value: Vec<Rec> = (0..4).map(sample).collect();
    value.write(&mut w);
    drop(w);
}
