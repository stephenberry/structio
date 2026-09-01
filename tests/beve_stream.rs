//! Streaming BEVE reads.
//!
//! The property that carries most of this file is that streaming and slurping
//! agree: whatever `from_beve` makes of a document, the streaming reader makes
//! of it too, and it does so however the bytes were cut up on the way in. That
//! is checked by pushing every document one byte at a time, which is the
//! smallest chunk there is and so exercises every boundary a real stream could
//! ever land on.
//!
//! The second property is that the window stays small. A streaming reader that
//! quietly buffered the whole file would pass every correctness test here, so
//! how much it holds unresolved is asserted directly. That is the framer's
//! half; the allocation it implies is checked against a real allocator in
//! `tests/memory.rs`, since a window that never compacted would keep this
//! figure small and the buffer large.

use std::collections::BTreeMap;
use std::io::{self, Read as _};

use structio::beve::{self, Documents, Feed, Mode};
use structio::{ErrorCode, StreamError};

#[derive(Default, Debug, PartialEq, Clone)]
struct Rec {
    id: u64,
    tag: String,
    ratio: f64,
    tags: Vec<String>,
    samples: Vec<f64>,
    flags: Vec<bool>,
    ok: bool,
}
structio::object!(Rec {
    id,
    tag,
    ratio,
    tags,
    samples,
    flags,
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

/// Reads any value at all, by stepping over it.
///
/// The streaming reader's job is to say where a value ends, and this is the
/// destination that cares about nothing else: it succeeds exactly when the span
/// the splitter named is one whole value and nothing more, whatever that value
/// happens to be.
#[derive(Default, Debug)]
struct Any;

impl<'de> beve::Read<'de> for Any {
    fn read<O: structio::Options>(
        &mut self,
        r: &mut beve::Reader<'de, O>,
    ) -> Result<(), ErrorCode> {
        r.skip_value()
    }
}

/// How many records to build.
///
/// The sweeps below feed a whole document one byte at a time, which makes them
/// quadratic in its length. Miri interprets rather than executes, so under it
/// the document shrinks: the boundaries are all still visited, there are just
/// fewer of them.
const fn records(n: u64) -> u64 {
    if cfg!(miri) { n / 10 + 1 } else { n }
}

fn sample(i: u64) -> Rec {
    Rec {
        id: i,
        // A multi-byte character, so a chunk boundary can land inside one.
        tag: format!("a\u{20ac}b{i}"),
        ratio: 1.5 * i as f64,
        tags: vec![String::new(), "x".into(), format!("{i}{i}")],
        samples: vec![0.5, 1.5, 2.5],
        // Nine, so the packed run is not a whole number of bytes.
        flags: (0..9).map(|b| (i + b).is_multiple_of(2)).collect(),
        ok: i.is_multiple_of(2),
    }
}

/// A stream of `n` whole documents, back to back.
fn concatenated(n: u64) -> Vec<u8> {
    (0..n).flat_map(|i| structio::to_beve(&sample(i))).collect()
}

/// An [`io::Read`] that hands out at most `chunk` bytes per call, so the reader
/// meets boundaries a `&[u8]` would never give it.
struct Chunked<'a> {
    data: &'a [u8],
    chunk: usize,
}

impl io::Read for Chunked<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.chunk.min(buf.len()).min(self.data.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        self.data = &self.data[n..];
        Ok(n)
    }
}

/// Every value a feed produces from `bytes`, pushed one byte at a time.
fn dribble<T>(mode: Mode, bytes: &[u8]) -> Vec<Result<T, StreamError>>
where
    T: for<'de> beve::Read<'de> + Default,
{
    let mut feed = Feed::new(mode);
    let mut out = Vec::new();
    for &b in bytes {
        feed.push(&[b]);
        while let Some(v) = feed.next_value::<T>() {
            out.push(v);
        }
    }
    feed.end();
    while let Some(v) = feed.next_value::<T>() {
        out.push(v);
    }
    out
}

// ---------------------------------------------------------------------------
// Documents back to back
// ---------------------------------------------------------------------------

#[test]
fn a_run_of_documents_reads_back_one_at_a_time() {
    let n = records(50);
    let bytes = concatenated(n);
    let mut docs = Documents::values(&bytes[..]);
    let got: Vec<Rec> = docs.iter::<Rec>().map(Result::unwrap).collect();
    assert_eq!(got, (0..n).map(sample).collect::<Vec<_>>());
}

#[test]
fn a_document_arriving_one_byte_at_a_time_reads_the_same() {
    let n = records(20);
    let bytes = concatenated(n);
    let got = dribble::<Rec>(Mode::Values, &bytes);
    assert_eq!(got.len() as u64, n);
    for (i, value) in got.into_iter().enumerate() {
        assert_eq!(value.unwrap(), sample(i as u64));
    }
}

#[test]
fn every_read_size_produces_the_same_values() {
    let n = records(20);
    let bytes = concatenated(n);
    for chunk in 1..=17 {
        let source = Chunked {
            data: &bytes,
            chunk,
        };
        let mut docs = Documents::values(source).read_size(chunk);
        let got: Vec<Rec> = docs.iter::<Rec>().map(Result::unwrap).collect();
        assert_eq!(got.len() as u64, n, "chunk {chunk}");
        assert_eq!(got[0], sample(0), "chunk {chunk}");
    }
}

#[test]
fn delimiters_between_documents_are_separators_rather_than_values() {
    let mut bytes = Vec::new();
    for i in 0..3u64 {
        // Before the first, between each pair, and after the last: a producer
        // that frames every record the same way is the likely one.
        bytes.push(structio::beve::header::DELIMITER);
        bytes.extend(structio::to_beve(&Small { id: i }));
    }
    bytes.push(structio::beve::header::DELIMITER);

    let mut docs = Documents::values(&bytes[..]);
    let got: Vec<u64> = docs.iter::<Small>().map(|r| r.unwrap().id).collect();
    assert_eq!(got, [0, 1, 2]);
}

#[test]
fn a_generic_array_claiming_more_elements_than_it_has_ends_in_an_error() {
    let mut bytes = structio::to_beve(&vec![Small { id: 1 }, Small { id: 2 }]);
    bytes[1] = 3 << 2;
    let mut docs = Documents::array(&bytes[..]);
    let got: Vec<Result<Small, _>> = docs.iter::<Small>().collect();
    assert_eq!(got.len(), 3);
    assert_eq!(
        got[2].as_ref().unwrap_err().as_parse().unwrap().code,
        ErrorCode::UnexpectedEnd
    );
}

#[test]
fn an_empty_stream_is_no_values_and_no_error() {
    for mode in [Mode::Values, Mode::Array] {
        let mut docs = small(Documents::new(&b""[..], mode));
        assert!(docs.next_value::<Small>().is_none(), "{mode:?}");
        // And the batch API is the one that says an empty input is not a
        // document, which is the divergence the module doc claims.
        assert!(structio::from_beve::<Small>(b"").is_err());
    }
}

// ---------------------------------------------------------------------------
// The elements of one array
// ---------------------------------------------------------------------------

#[test]
fn a_generic_array_hands_out_its_elements() {
    let n = records(50);
    let all: Vec<Rec> = (0..n).map(sample).collect();
    let bytes = structio::to_beve(&all);

    let mut docs = Documents::array(&bytes[..]);
    let got: Vec<Rec> = docs.iter::<Rec>().map(Result::unwrap).collect();
    assert_eq!(got, all);
}

#[test]
fn a_generic_array_arriving_one_byte_at_a_time_reads_the_same() {
    let n = records(20);
    let all: Vec<Rec> = (0..n).map(sample).collect();
    let bytes = structio::to_beve(&all);
    let got = dribble::<Rec>(Mode::Array, &bytes);
    assert_eq!(got.len(), all.len());
    for (value, want) in got.into_iter().zip(&all) {
        assert_eq!(&value.unwrap(), want);
    }
}

#[test]
fn a_typed_numeric_array_hands_out_its_elements() {
    let all: Vec<f64> = (0..100).map(|i| i as f64 * 0.25).collect();
    let bytes = structio::to_beve(&all);
    // The point of the exercise: this is one header and one block, not a
    // hundred values, and it still comes back one element at a time.
    let mut docs = Documents::array(&bytes[..]);
    let got: Vec<f64> = docs.iter::<f64>().map(Result::unwrap).collect();
    assert_eq!(got, all);
}

#[test]
fn a_typed_array_of_every_width_hands_out_its_elements() {
    // One case per stored width and signedness, read back at the same type and
    // then at a wider one, since the implied header is what decides both.
    assert_eq!(
        elements::<u8>(&structio::to_beve(&vec![1u8, 2, 255])),
        [1, 2, 255]
    );
    assert_eq!(
        elements::<i64>(&structio::to_beve(&vec![-1i8, 0, 127])),
        [-1, 0, 127]
    );
    assert_eq!(
        elements::<u32>(&structio::to_beve(&vec![1u16, 65535])),
        [1, 65535]
    );
    assert_eq!(
        elements::<i64>(&structio::to_beve(&vec![i32::MIN, 7])),
        [i32::MIN as i64, 7]
    );
    assert_eq!(
        elements::<f64>(&structio::to_beve(&vec![1.5f32, -2.5])),
        [1.5, -2.5]
    );
    assert_eq!(
        elements::<u64>(&structio::to_beve(&vec![u64::MAX, 0])),
        [u64::MAX, 0]
    );
}

fn elements<T>(bytes: &[u8]) -> Vec<T>
where
    T: for<'de> beve::Read<'de> + Default,
{
    small(Documents::array(bytes))
        .iter::<T>()
        .map(Result::unwrap)
        .collect()
}

/// A reader that asks for a little at a time.
///
/// Two reasons, and the second is why it is a helper rather than a habit. It
/// meets more chunk boundaries than the default 64 KiB ask ever would on a
/// document this size. And it keeps the tests that build a reader in a loop
/// cheap under Miri, where filling initializes the read buffer once per reader
/// and a 64 KiB memset is 65,536 tracked writes rather than one instruction.
fn small<R: io::Read>(docs: Documents<R>) -> Documents<R> {
    docs.read_size(64)
}

#[test]
fn a_packed_boolean_array_hands_out_its_bits() {
    for n in 0..40usize {
        let all: Vec<bool> = (0..n).map(|i| i % 3 == 0).collect();
        let bytes = structio::to_beve(&all);
        assert_eq!(elements::<bool>(&bytes), all, "{n} booleans");
    }
}

#[test]
fn a_typed_string_array_hands_out_its_strings() {
    let all: Vec<String> = vec!["".into(), "a".into(), "\u{20ac}\u{1f600}".into()];
    let bytes = structio::to_beve(&all);
    assert_eq!(elements::<String>(&bytes), all);
}

/// An aligned typed array of `values`, which no writer here emits.
///
/// The form states its element type in a second header and pads the payload so
/// a reader can point straight at it, and the splitter has to step over both to
/// reach element zero.
fn aligned(values: &[f64]) -> Vec<u8> {
    let mut bytes = vec![
        structio::beve::header::ALIGNED_ARRAY,
        structio::beve::header::array_of(structio::beve::header::CAT_FLOAT, 3),
    ];
    let mut size = [0u8; 8];
    let used = structio::beve::header::encode_size(values.len() as u64, &mut size);
    bytes.extend_from_slice(&size[..used]);
    bytes.push(5);
    bytes.extend_from_slice(&[0; 5]);
    for v in values {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes
}

#[test]
fn an_aligned_typed_array_hands_out_its_elements() {
    let values = [1.5f64, 2.5, 3.5];
    let bytes = aligned(&values);
    assert!(structio::validate_beve(&bytes).is_ok());
    assert_eq!(elements::<f64>(&bytes), values);
}

#[test]
fn a_typed_array_arriving_one_byte_at_a_time_reads_the_same() {
    for bytes in [
        structio::to_beve(&(0..30).map(|i| i as f64).collect::<Vec<f64>>()),
        structio::to_beve(&(0..30).map(|i| i % 5 == 0).collect::<Vec<bool>>()),
        structio::to_beve(&(0..30).map(|i| i.to_string()).collect::<Vec<String>>()),
    ] {
        let got = dribble::<Any>(Mode::Array, &bytes);
        assert_eq!(got.len(), 30, "{bytes:02x?}");
    }
}

#[test]
fn an_empty_array_is_no_values_and_no_error() {
    for bytes in [
        structio::to_beve(&Vec::<Rec>::new()),
        structio::to_beve(&Vec::<f64>::new()),
        structio::to_beve(&Vec::<bool>::new()),
        structio::to_beve(&Vec::<String>::new()),
    ] {
        let mut docs = small(Documents::array(&bytes[..]));
        assert!(docs.next_value::<Small>().is_none(), "{bytes:02x?}");
    }
}

#[test]
fn an_array_claiming_more_elements_than_it_has_ends_in_an_error() {
    // A count comes off the wire and need not describe what follows it. The
    // elements that are there still arrive, and then the stream stops with the
    // truncation rather than waiting on bytes that are not coming.
    let mut bytes = structio::to_beve(&vec![1u32, 2, 3]);
    // Rewrite the count in place: same width, one element more.
    bytes[1] = 4 << 2;
    let mut docs = Documents::array(&bytes[..]);
    let got: Vec<Result<u32, _>> = docs.iter::<u32>().collect();
    assert_eq!(got.len(), 4);
    assert_eq!(*got[0].as_ref().unwrap(), 1);
    assert_eq!(
        got[3].as_ref().unwrap_err().as_parse().unwrap().code,
        ErrorCode::UnexpectedEnd
    );
}

#[test]
fn a_top_level_value_that_is_not_an_array_is_refused() {
    let bytes = structio::to_beve(&Small { id: 1 });
    let mut docs = Documents::array(&bytes[..]);
    let err = docs.next_value::<Small>().unwrap().unwrap_err();
    assert_eq!(err.as_parse().unwrap().code, ErrorCode::ExpectedArray);
}

#[test]
fn bytes_after_the_array_are_trailing_content() {
    let mut bytes = structio::to_beve(&vec![1u32, 2, 3]);
    bytes.extend(structio::to_beve(&Small { id: 9 }));
    let mut docs = Documents::array(&bytes[..]);
    let got: Vec<Result<u32, _>> = docs.iter::<u32>().collect();
    // Three good elements, then the failure, and then the stream is over.
    assert_eq!(got.len(), 4);
    assert_eq!(
        got[3].as_ref().unwrap_err().as_parse().unwrap().code,
        ErrorCode::TrailingContent
    );
}

// ---------------------------------------------------------------------------
// What streaming costs, and what it does not
// ---------------------------------------------------------------------------

#[test]
fn the_window_stays_bounded_over_a_long_stream() {
    let n = records(2000);
    let all: Vec<Small> = (0..n).map(|id| Small { id }).collect();
    let bytes = structio::to_beve(&all);
    assert!(bytes.len() > 8000 || cfg!(miri));

    let mut docs = Documents::array(&bytes[..]).read_size(64);
    let mut peak = 0;
    let mut count = 0;
    while let Some(value) = docs.next_value_into(&mut Small::default()) {
        value.unwrap();
        peak = peak.max(docs.buffered());
        count += 1;
    }
    assert_eq!(count, n);
    // One record plus one read, not the file. The bound is generous because
    // compaction is amortized; what it rules out is the live window growing
    // with `n`, which is what a framer that failed to consume would do.
    assert!(peak < 1024, "buffered {peak} bytes at peak");
}

#[test]
fn a_typed_array_larger_than_the_window_streams_anyway() {
    let n = if cfg!(miri) { 200 } else { 20_000 };
    let all: Vec<f64> = (0..n).map(|i| i as f64).collect();
    let bytes = structio::to_beve(&all);

    let mut docs = Documents::array(&bytes[..]).read_size(64);
    let mut peak = 0;
    let mut sum = 0.0;
    while let Some(value) = docs.next_value::<f64>() {
        sum += value.unwrap();
        peak = peak.max(docs.buffered());
    }
    assert_eq!(sum, all.iter().sum::<f64>());
    assert!(peak < 1024, "buffered {peak} bytes at peak");
}

#[test]
fn a_borrowed_field_points_into_the_window() {
    let bytes = structio::to_beve(&vec![Borrowed { tag: "first" }, Borrowed { tag: "second" }]);
    let mut docs = Documents::array(&bytes[..]);
    // One at a time, because the borrow holds the window still.
    let first: Borrowed = docs.next_value().unwrap().unwrap();
    assert_eq!(first.tag, "first");
    let second: Borrowed = docs.next_value().unwrap().unwrap();
    assert_eq!(second.tag, "second");
}

#[test]
fn reading_into_an_existing_value_keeps_its_allocations() {
    let n = records(30);
    let all: Vec<Rec> = (0..n).map(sample).collect();
    let bytes = structio::to_beve(&all);

    let mut docs = Documents::array(&bytes[..]);
    let mut value = Rec::default();
    let mut count = 0;
    while let Some(result) = docs.next_value_into(&mut value) {
        result.unwrap();
        assert_eq!(value, all[count]);
        count += 1;
    }
    assert_eq!(count as u64, n);
}

#[test]
fn the_offset_tracks_the_stream_rather_than_the_window() {
    let bytes = concatenated(records(30));
    let mut docs = Documents::values(&bytes[..]).read_size(7);
    assert_eq!(docs.offset(), 0);
    let one = structio::to_beve(&sample(0));
    docs.next_value_into(&mut Rec::default()).unwrap().unwrap();
    assert_eq!(docs.offset(), one.len());
    docs.next_value_into(&mut Rec::default()).unwrap().unwrap();
    assert_eq!(
        docs.offset(),
        one.len() + structio::to_beve(&sample(1)).len()
    );
}

#[test]
fn into_parts_hands_back_what_was_read_but_not_used() {
    let bytes = concatenated(3);
    let mut docs = Documents::values(&bytes[..]);
    docs.next_value_into(&mut Rec::default()).unwrap().unwrap();
    let (rest, unread) = docs.into_parts();

    let mut tail = unread;
    let mut remaining = Vec::new();
    let mut rest = rest;
    rest.read_to_end(&mut remaining).unwrap();
    tail.extend(remaining);
    // The two halves put back together are exactly the rest of the stream.
    assert_eq!(tail, bytes[structio::to_beve(&sample(0)).len()..]);
}

// ---------------------------------------------------------------------------
// Failure
// ---------------------------------------------------------------------------

#[test]
fn a_record_that_does_not_match_the_type_is_reported_and_skipped() {
    // A well-formed document whose second element is a string where a struct
    // was asked for: the framing is fine, so the third element still arrives.
    let mut bytes = vec![structio::beve::header::GENERIC_ARRAY];
    let mut size = [0u8; 8];
    let used = structio::beve::header::encode_size(3, &mut size);
    bytes.extend_from_slice(&size[..used]);
    bytes.extend(structio::to_beve(&Small { id: 1 }));
    bytes.extend(structio::to_beve(&String::from("not a record")));
    bytes.extend(structio::to_beve(&Small { id: 3 }));
    assert!(structio::validate_beve(&bytes).is_ok());

    let mut docs = Documents::array(&bytes[..]);
    let got: Vec<Result<Small, _>> = docs.iter::<Small>().collect();
    assert_eq!(got.len(), 3);
    assert_eq!(got[0].as_ref().unwrap().id, 1);
    assert_eq!(
        got[1].as_ref().unwrap_err().as_parse().unwrap().code,
        ErrorCode::ExpectedObject
    );
    assert_eq!(got[2].as_ref().unwrap().id, 3);
}

#[test]
fn a_truncated_document_is_an_unexpected_end() {
    let bytes = structio::to_beve(&sample(0));
    for cut in 1..bytes.len() {
        let mut docs = small(Documents::values(&bytes[..cut]));
        let err = docs
            .next_value_into(&mut Rec::default())
            .unwrap()
            .unwrap_err();
        assert_eq!(
            err.as_parse().unwrap().code,
            ErrorCode::UnexpectedEnd,
            "cut at {cut}"
        );
        // And terminal: there is no position left to resume from.
        assert!(docs.next_value_into(&mut Rec::default()).is_none());
    }
}

#[test]
fn a_framing_failure_is_reported_once_and_ends_the_stream() {
    // Type 7 is not a type at all, so the walk cannot say where the value ends.
    let bytes = [0b0000_0111u8, 0, 0, 0];
    let mut docs = Documents::values(&bytes[..]);
    let err = docs.next_value::<Small>().unwrap().unwrap_err();
    assert_eq!(err.as_parse().unwrap().code, ErrorCode::InvalidHeader);
    assert!(docs.next_value::<Small>().is_none());
    // Nothing is retained past the failure, and pushing more cannot grow it.
    assert_eq!(docs.buffered(), 0);
}

#[test]
fn a_length_the_producer_never_delivers_hits_the_limit() {
    // A string claiming a megabyte, with nothing behind it.
    let mut bytes = vec![structio::beve::header::STRING];
    let mut size = [0u8; 8];
    let used = structio::beve::header::encode_size(1 << 20, &mut size);
    bytes.extend_from_slice(&size[..used]);

    let mut feed = Feed::values().max_value(4096);
    feed.push(&bytes);
    feed.push(&vec![b'x'; 8192]);
    let err = feed.next_value::<String>().unwrap().unwrap_err();
    assert_eq!(err.as_parse().unwrap().code, ErrorCode::DocumentTooLarge);
}

#[test]
fn a_limit_does_not_stop_a_stream_of_small_values() {
    let bytes = concatenated(records(30));
    // The records are not all the same length, and the limit is on the largest
    // single value rather than on the stream.
    let largest = (0..records(30))
        .map(|i| structio::to_beve(&sample(i)).len())
        .max()
        .unwrap();
    let mut docs = Documents::values(&bytes[..]).max_value(largest);
    let got: Vec<Rec> = docs.iter::<Rec>().map(Result::unwrap).collect();
    assert_eq!(got.len() as u64, records(30));
}

#[test]
fn pushing_at_a_dead_feed_cannot_grow_it() {
    let mut feed = Feed::values();
    feed.push(&[0b0000_0111]);
    assert!(feed.next_value::<Small>().unwrap().is_err());
    for _ in 0..100 {
        feed.push(&vec![0u8; 1024]);
    }
    assert_eq!(feed.buffered(), 0);
    assert!(feed.next_value::<Small>().is_none());
}

/// A container holding one value: the bytes that open it, and whatever has to
/// follow the value to close it.
///
/// One per construct that charges a level, so the depth agreement below is
/// checked wherever the two walks could disagree about it rather than only
/// where it is easiest to build.
struct Wrap {
    name: &'static str,
    open: &'static [u8],
    close: &'static [u8],
}

const EXTENSION: u8 = structio::beve::header::TY_EXTENSION;
const TYPE_TAG: u8 = (structio::beve::header::EXT_TYPE_TAG << 3) | EXTENSION;
const MATRIX: u8 = (structio::beve::header::EXT_MATRIX << 3) | EXTENSION;
const COMPLEX: u8 = (structio::beve::header::EXT_COMPLEX << 3) | EXTENSION;

const WRAPS: [Wrap; 4] = [
    Wrap {
        name: "generic array",
        open: &[structio::beve::header::GENERIC_ARRAY, 1 << 2],
        close: &[],
    },
    Wrap {
        name: "object",
        // One member, whose key is the empty string.
        open: &[structio::beve::header::OBJECT, 1 << 2, 0],
        close: &[],
    },
    Wrap {
        name: "type tag",
        // The deprecated tag: an index, then the value it tagged.
        open: &[TYPE_TAG, 0],
        close: &[],
    },
    Wrap {
        // A layout byte and two values, so the second value is what closes it.
        name: "matrix",
        open: &[MATRIX, 0],
        close: &[structio::beve::header::NULL],
    },
];

/// Nesting depths to try.
///
/// Every one of them ordinarily. Under Miri only the ends: the answer can only
/// change at the limit, and walking the middle of the range costs minutes there
/// to confirm what the two ends already pin.
fn depths() -> Vec<usize> {
    let limit = structio::beve::reader::MAX_DEPTH as usize;
    if cfg!(miri) {
        vec![0, 1, 2, limit - 2, limit - 1, limit, limit + 1]
    } else {
        (0..limit + 2).collect()
    }
}

/// `depth` copies of `wrap` around `inner`.
fn nested(wrap: &Wrap, depth: usize, inner: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for _ in 0..depth {
        bytes.extend_from_slice(wrap.open);
    }
    bytes.extend_from_slice(inner);
    for _ in 0..depth {
        bytes.extend_from_slice(wrap.close);
    }
    bytes
}

/// Did the splitter name a whole document, or refuse to?
///
/// The distinction matters because the reader gets a second opinion: it walks
/// the span it is handed and applies the depth limit again from zero, so a
/// splitter that framed a document too deep would still produce an error, just
/// not its own. What separates the two is how far the stream advanced.
fn frames(bytes: &[u8]) -> bool {
    let mut feed = Feed::values();
    feed.push(bytes);
    feed.end();
    let read = feed.next_value::<Any>();
    let framed = feed.offset() == bytes.len();
    if framed {
        // Whatever the splitter frames, `Any` reads: the span is one whole
        // value, so stepping over it lands exactly on its end.
        assert!(
            read.is_some_and(|r| r.is_ok()),
            "framed but not readable: {bytes:02x?}"
        );
    }
    framed
}

#[test]
fn nesting_past_the_limit_is_a_framing_failure() {
    let deepest = structio::beve::reader::MAX_DEPTH as usize;
    let null = [structio::beve::header::NULL];
    let mut feed = Feed::values();
    feed.push(&nested(&WRAPS[0], deepest + 1, &null));
    feed.end();
    let err = feed.next_value::<Small>().unwrap().unwrap_err();
    assert_eq!(err.as_parse().unwrap().code, ErrorCode::ExceededMaxDepth);
    // The splitter's own refusal, not the reader's: nothing was consumed.
    assert_eq!(feed.offset(), 0);
}

#[test]
fn the_splitter_frames_exactly_what_the_reader_reads() {
    // The two walks have to agree about where a value ends, and the deepest
    // accepted document is where they would first disagree. Every container
    // that charges a level is wrapped in turn, and both inner values are tried,
    // because they charge for different things: a scalar costs nothing, and a
    // typed array costs a level despite never recursing.
    let inners = [
        vec![structio::beve::header::NULL],
        structio::to_beve(&vec![1u8, 2, 3]),
    ];
    for wrap in &WRAPS {
        for inner in &inners {
            for depth in depths() {
                let bytes = nested(wrap, depth, inner);
                assert_eq!(
                    frames(&bytes),
                    structio::validate_beve(&bytes).is_ok(),
                    "{depth} of {} around {inner:02x?}",
                    wrap.name
                );
            }
        }
    }
}

#[test]
fn the_extensions_are_framed_even_though_they_are_not_read() {
    // None of these become Rust types, but all state their own extent, so a
    // stream carrying one stays readable for the documents around it.
    // A complex value's inner header puts the class and byte count where a
    // number header does, and spends the type field on whether this is one
    // number or a run of them.
    let pair = structio::beve::header::number(structio::beve::header::CAT_FLOAT, 3) & !0b111;
    let mut docs: Vec<Vec<u8>> = vec![
        // A delimiter used as a value rather than as a separator, which is
        // where a document may legitimately hold one.
        nested(&WRAPS[0], 1, &[structio::beve::header::DELIMITER]),
        // One complex number, and then a run of two.
        [&[COMPLEX, pair][..], &[0; 16]].concat(),
        [&[COMPLEX, pair | 1, 2 << 2][..], &[0; 32]].concat(),
    ];
    // A matrix, whose two operands are typed arrays.
    docs.push(
        [
            &[MATRIX, 0][..],
            &structio::to_beve(&vec![2u32, 3]),
            &structio::to_beve(&vec![1.0f64, 2.0, 3.0, 4.0, 5.0, 6.0]),
        ]
        .concat(),
    );

    for bytes in docs {
        assert!(structio::validate_beve(&bytes).is_ok(), "{bytes:02x?}");
        assert!(frames(&bytes), "{bytes:02x?}");
        // And cut anywhere at all, it is still exactly one value.
        assert_eq!(
            dribble::<Any>(Mode::Values, &bytes).len(),
            1,
            "{bytes:02x?}"
        );
    }
}

#[test]
fn an_io_failure_is_an_io_error_rather_than_a_parse_error() {
    struct Broken;
    impl io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::ConnectionReset, "gone"))
        }
    }
    let mut docs = Documents::values(Broken);
    let err = docs
        .next_value_into(&mut Small::default())
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, StreamError::Io(_)));
    assert_eq!(err.as_io().unwrap().kind(), io::ErrorKind::ConnectionReset);
}

#[test]
fn an_interrupted_read_is_retried() {
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
    let bytes = concatenated(3);
    let mut docs = Documents::values(Flaky {
        data: &bytes,
        interrupt: false,
    });
    let got: Vec<Rec> = docs.iter::<Rec>().map(Result::unwrap).collect();
    assert_eq!(got.len(), 3);
}

// ---------------------------------------------------------------------------
// Streamed and slurped agree
// ---------------------------------------------------------------------------

#[test]
fn every_document_shape_streams_to_what_it_slurps_to() {
    // One document per shape the walk has an arm for, each cut at every byte.
    let mut docs: Vec<Vec<u8>> = vec![
        structio::to_beve(&()),
        structio::to_beve(&true),
        structio::to_beve(&0u8),
        structio::to_beve(&-1i64),
        structio::to_beve(&1.5f32),
        structio::to_beve(&f64::NAN),
        structio::to_beve(&"text"),
        structio::to_beve(&String::new()),
        structio::to_beve(&vec![1u8, 2, 3]),
        structio::to_beve(&vec![true, false, true]),
        structio::to_beve(&vec!["a".to_string(), "b".into()]),
        structio::to_beve(&Vec::<Rec>::new()),
        structio::to_beve(&sample(7)),
        structio::to_beve(&vec![sample(1), sample(2)]),
        aligned(&[1.5, 2.5, 3.5]),
    ];
    let mut map: BTreeMap<u16, Vec<f64>> = BTreeMap::new();
    map.insert(1, vec![1.0]);
    map.insert(65535, vec![]);
    docs.push(structio::to_beve(&map));
    let mut names: BTreeMap<String, Small> = BTreeMap::new();
    names.insert("a".into(), Small { id: 1 });
    docs.push(structio::to_beve(&names));

    for bytes in docs {
        assert!(structio::validate_beve(&bytes).is_ok(), "{bytes:02x?}");
        // The boundary the splitter finds is the whole document, and it finds
        // the same one however the bytes were cut on the way in.
        assert!(frames(&bytes), "{bytes:02x?}");
        let got = dribble::<Any>(Mode::Values, &bytes);
        assert_eq!(got.len(), 1, "{bytes:02x?}");
        assert!(got[0].is_ok(), "{bytes:02x?}");
    }
}
