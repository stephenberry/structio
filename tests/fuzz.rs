//! Property tests over generated documents.
//!
//! The invariant that matters most: for any value, `to_string` then `from_str`
//! must reproduce it exactly, and re-serializing must give byte-identical
//! output. That catches writer and parser bugs that agree with each other only
//! by accident.

use std::collections::BTreeMap;
use structio::json::minify_with;
use structio::{
    AllowComments, Complex, ErrorCode, Matrix, MatrixLayout, Options, Pretty, PrettyInlineArrays,
    Standard, from_str, minify, prettify, prettify_with, to_string, to_string_with,
};

/// How many rounds a randomized test should draw.
///
/// Miri interprets rather than executes, at hundreds of times the cost per
/// round, so under it these become samples. Every code path a round can take
/// is reachable within the first few, so the sample is still a real check of
/// the unsafe in the writers and the key map.
const fn rounds(n: u32) -> u32 {
    if cfg!(miri) { n / 100 + 1 } else { n }
}

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
    fn string(&mut self, max: usize) -> String {
        // Deliberately includes characters that must be escaped, multi-byte
        // UTF-8, and characters outside the basic plane.
        const POOL: &[char] = &[
            'a', 'b', 'Z', '0', '9', ' ', '"', '\\', '/', '\n', '\r', '\t', '\u{8}', '\u{c}',
            '\u{1}', '\u{1f}', 'é', 'ß', '中', '文', '😀', '🎉', '\u{7f}', '~', '{', '}', '[', ']',
            ':', ',',
        ];
        let n = self.below(max as u64 + 1) as usize;
        (0..n)
            .map(|_| POOL[self.below(POOL.len() as u64) as usize])
            .collect()
    }
    fn float(&mut self) -> f64 {
        match self.below(6) {
            0 => f64::from_bits(self.next()),
            1 => self.below(1000) as f64 / 8.0,
            2 => self.below(1_000_000) as f64,
            3 => -(self.below(1_000_000) as f64) / 3.0,
            4 => (self.below(1000) as f64) * 1e-300,
            _ => (self.below(1000) as f64) * 1e300,
        }
    }
}

#[derive(Default, Debug, PartialEq)]
struct Leaf {
    flag: bool,
    count: i64,
    ratio: f64,
    label: String,
}
structio::object!(Leaf {
    flag,
    count,
    ratio,
    label
});

/// Declared positionally, so every property below covers the `array!` path as
/// well: elements found by counting rather than by key, and a length that has
/// to match exactly or fail.
#[derive(Default, Debug, PartialEq)]
struct Span {
    start: u32,
    end: u32,
    label: String,
}
structio::array!(Span [start, end, label]);

/// Homogeneous, so BEVE stores it as a typed array. Reading one back into a
/// positional struct is its own path, and the corruption properties below are
/// where it gets exercised on malformed input.
#[derive(Default, Debug, PartialEq, Clone, Copy)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}
structio::array!(Rgb [u8; r, g, b]);

/// An enum whose variants carry nothing, so the value on the wire is a name.
#[derive(Default, Debug, PartialEq)]
enum Kind {
    #[default]
    Plain,
    Marked,
    Hidden,
}
structio::unit_enum!(Kind {
    Plain,
    Marked,
    Hidden
});

/// An enum whose variants carry values, so the value on the wire is an object
/// of one member keyed by the name. Both wire forms are therefore in every
/// property below, and a payload reaches for a struct, a scalar and a string in
/// turn, so a tag over each of the three is covered.
#[derive(Default, Debug, PartialEq)]
enum Choice {
    #[default]
    Nothing,
    Leaf(Leaf),
    Count(i64),
    Text(String),
}
structio::tagged_enum!(Choice {
    Nothing,
    Leaf(_),
    Count(_),
    Text(_),
});

#[derive(Default, Debug, PartialEq)]
struct Node {
    name: String,
    tags: Vec<String>,
    numbers: Vec<f64>,
    // A narrower float than the reader's own widening path uses, so that a
    // typed array whose elements are not `f64` is in every property below.
    gains: Vec<f32>,
    leaves: Vec<Leaf>,
    lookup: BTreeMap<String, i32>,
    maybe: Option<Leaf>,
    fixed: [u16; 4],
    span: Span,
    color: Rgb,
    // The two BEVE extensions that carry data, so every property below covers
    // them: a lone complex value, a run of them stored as one block, and a
    // matrix, whose own data is a third array form again.
    phase: Complex<f64>,
    signal: Vec<Complex<f32>>,
    grid: Matrix<f64>,
    // The two enum forms, and a run of each: a run of unit variants is a BEVE
    // string array, and a run of tags a generic one, so both sequence forms are
    // covered as well as a variant standing alone.
    kind: Kind,
    kinds: Vec<Kind>,
    choice: Choice,
    choices: Vec<Choice>,
}
structio::object!(Node {
    name,
    tags,
    numbers,
    gains,
    leaves,
    lookup,
    maybe,
    fixed,
    span,
    color,
    phase,
    signal,
    grid,
    kind,
    kinds,
    choice,
    choices
});

/// A float that has a JSON form. NaN and infinity are written as null, so they
/// cannot round trip and are tested separately.
fn finite(r: &mut Rng) -> f64 {
    let f = r.float();
    if f.is_finite() { f } else { 0.5 }
}

/// The same, narrowed. A finite `f64` need not stay finite as an `f32`: the
/// generator reaches 1e300, which overflows to infinity on the way down.
fn finite32(r: &mut Rng) -> f32 {
    let f = finite(r) as f32;
    if f.is_finite() { f } else { 0.25 }
}

fn gen_leaf(r: &mut Rng) -> Leaf {
    Leaf {
        flag: r.below(2) == 0,
        count: r.next() as i64,
        // NaN and infinity have no JSON form and are written as null, so they
        // cannot round trip. Exclude them here and test them separately.
        ratio: {
            let mut f = r.float();
            if !f.is_finite() {
                f = 0.5;
            }
            f
        },
        label: r.string(20),
    }
}

fn gen_node(r: &mut Rng) -> Node {
    Node {
        name: r.string(30),
        tags: (0..r.below(6)).map(|_| r.string(12)).collect(),
        numbers: (0..r.below(8))
            .map(|_| {
                let f = r.float();
                if f.is_finite() { f } else { 1.0 }
            })
            .collect(),
        leaves: (0..r.below(4)).map(|_| gen_leaf(r)).collect(),
        lookup: (0..r.below(5))
            .map(|_| (r.string(8), r.next() as i32))
            .collect(),
        maybe: if r.below(3) == 0 {
            None
        } else {
            Some(gen_leaf(r))
        },
        fixed: [
            r.next() as u16,
            r.next() as u16,
            r.next() as u16,
            r.next() as u16,
        ],
        span: Span {
            start: r.next() as u32,
            end: r.next() as u32,
            label: r.string(10),
        },
        color: Rgb {
            r: r.next() as u8,
            g: r.next() as u8,
            b: r.next() as u8,
        },
        phase: Complex::new(finite(r), finite(r)),
        gains: (0..r.below(6)).map(|_| finite32(r)).collect(),
        signal: (0..r.below(5))
            .map(|_| Complex::new(finite32(r), finite32(r)))
            .collect(),
        grid: {
            // Rank zero, a dimension of zero, and an ordinary shape all turn up,
            // which is the whole range a matrix can legally hold.
            let extents: Vec<usize> = (0..r.below(3)).map(|_| r.below(4) as usize).collect();
            let n = if extents.is_empty() {
                0
            } else {
                extents.iter().product()
            };
            let layout = if r.below(2) == 0 {
                MatrixLayout::RowMajor
            } else {
                MatrixLayout::ColumnMajor
            };
            Matrix::new(layout, extents, (0..n).map(|_| finite(r)).collect()).expect("shape")
        },
        kind: gen_kind(r),
        kinds: (0..r.below(5)).map(|_| gen_kind(r)).collect(),
        choice: gen_choice(r),
        choices: (0..r.below(4)).map(|_| gen_choice(r)).collect(),
    }
}

fn gen_kind(r: &mut Rng) -> Kind {
    match r.below(3) {
        0 => Kind::Plain,
        1 => Kind::Marked,
        _ => Kind::Hidden,
    }
}

fn gen_choice(r: &mut Rng) -> Choice {
    match r.below(4) {
        0 => Choice::Nothing,
        1 => Choice::Leaf(gen_leaf(r)),
        2 => Choice::Count(r.next() as i64),
        _ => Choice::Text(r.string(12)),
    }
}

#[test]
fn round_trip_is_exact_and_stable() {
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    for i in 0..rounds(20_000) {
        let node = gen_node(&mut r);
        let json = to_string(&node);
        let back: Node = match from_str(&json) {
            Ok(v) => v,
            Err(e) => panic!("iteration {i}: {} in {json}", e.display_with(&json)),
        };
        assert_eq!(back, node, "iteration {i} via {json}");
        // Writing the parsed value must reproduce the same bytes.
        assert_eq!(to_string(&back), json, "iteration {i} was not stable");
    }
}

#[test]
fn reading_into_a_reused_value_matches_a_fresh_one() {
    // The reuse path takes different branches (existing elements are read over
    // rather than pushed), so it needs its own coverage.
    let mut r = Rng(0x1234_5678_9ABC_DEF0);
    let mut reused = Node::default();
    for i in 0..rounds(5_000) {
        let node = gen_node(&mut r);
        let json = to_string(&node);
        structio::read_into(&mut reused, &json).expect("parse");
        assert_eq!(reused, node, "iteration {i} via {json}");
    }
}

#[test]
fn truncating_any_document_never_panics() {
    // Every prefix of a valid document must produce an error, not a panic and
    // not a wrong success.
    let mut r = Rng(0xFEED_FACE_CAFE_BEEF);
    for _ in 0..rounds(300) {
        let json = to_string(&gen_node(&mut r));
        for cut in 0..json.len() {
            if !json.is_char_boundary(cut) {
                continue;
            }
            let _ = from_str::<Node>(&json[..cut]);
        }
    }
}

#[test]
fn corrupting_any_byte_never_panics() {
    let mut r = Rng(0x0BAD_C0DE_0BAD_C0DE);
    for _ in 0..rounds(2_000) {
        let json = to_string(&gen_node(&mut r));
        let mut bytes = json.into_bytes();
        if bytes.is_empty() {
            continue;
        }
        let pos = (r.below(bytes.len() as u64)) as usize;
        // Usually an ASCII byte, since that is where the structure is. Now and
        // then a non-ASCII one, which is the class the word scan has to hold
        // back from matching and which the writers may not carry literally.
        // It goes in as a whole character, because a lone continuation byte
        // would only fail the check below and never be seen.
        if r.below(8) == 0 {
            let c = ['\u{e9}', '\u{20ac}', '\u{1f600}', '\u{80}'][r.below(4) as usize];
            let mut buf = [0u8; 4];
            let wide = c.encode_utf8(&mut buf).as_bytes().to_vec();
            bytes.splice(pos..pos + 1, wide);
        } else {
            bytes[pos] = (r.below(128)) as u8;
        }
        if let Ok(s) = std::str::from_utf8(&bytes) {
            let _ = from_str::<Node>(s);
        }
    }
}

#[test]
fn non_finite_floats_become_null() {
    #[derive(Default, Debug, PartialEq)]
    struct F {
        v: f64,
    }
    structio::object!(F { v });

    for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        // JSON has no way to spell these, so they are written as null, which is
        // what Glaze does.
        assert_eq!(to_string(&F { v }), r#"{"v":null}"#);
    }
    // And null reads back as a value the field keeps, rather than an error.
    assert!(from_str::<F>(r#"{"v":null}"#).is_err());
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

/// A reader that hands out at most `chunk` bytes per call.
struct Choppy<'a> {
    data: &'a [u8],
    chunk: usize,
}

impl std::io::Read for Choppy<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.chunk.min(buf.len()).min(self.data.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        self.data = &self.data[n..];
        Ok(n)
    }
}

#[test]
fn draining_to_a_sink_reproduces_the_in_memory_bytes() {
    // The generated strings are full of the bytes a structural writer could
    // trip over: braces, brackets, quotes, backslashes, newlines. A drain must
    // be invisible in the output regardless of where it lands.
    let mut r = Rng(0x51ED_1234_ABCD_0001);
    for i in 0..rounds(3_000) {
        let node = gen_node(&mut r);
        let want = to_string(&node);
        let cap = 1 + r.below(want.len().max(1) as u64 + 8) as usize;
        let mut got = Vec::new();
        structio::to_writer_buffered(&node, &mut got, cap).unwrap();
        assert_eq!(
            String::from_utf8(got).unwrap(),
            want,
            "iteration {i} at buffer size {cap}"
        );
    }
}

#[test]
fn streamed_reads_recover_the_values_at_any_chunking() {
    let mut r = Rng(0x51ED_1234_ABCD_0002);
    for i in 0..rounds(600) {
        let nodes: Vec<Node> = (0..1 + r.below(4)).map(|_| gen_node(&mut r)).collect();

        // The three framings, built from the same values.
        let lines: String = nodes.iter().map(|n| to_string(n) + "\n").collect();
        let array = to_string(&nodes);
        let values: Vec<String> = nodes.iter().map(to_string).collect();
        let values = values.join(" ");

        let chunk = 1 + r.below(40) as usize;
        for (mode, text) in [
            (structio::Mode::Lines, &lines),
            (structio::Mode::Array, &array),
            (structio::Mode::Values, &values),
        ] {
            let src = Choppy {
                data: text.as_bytes(),
                chunk,
            };
            let mut docs = structio::Documents::new(src, mode).read_size(chunk);
            let got: Vec<Node> = docs
                .iter::<Node>()
                .map(|v| match v {
                    Ok(v) => v,
                    Err(e) => panic!("iteration {i} {mode:?} chunk {chunk}: {e} in {text}"),
                })
                .collect();
            assert_eq!(got, nodes, "iteration {i} {mode:?} chunk {chunk}");
        }
    }
}

#[test]
fn a_fed_stream_matches_a_pulled_one_byte_for_byte() {
    let mut r = Rng(0x51ED_1234_ABCD_0003);
    for i in 0..rounds(400) {
        let nodes: Vec<Node> = (0..1 + r.below(3)).map(|_| gen_node(&mut r)).collect();
        let text: String = nodes.iter().map(|n| to_string(n) + "\n").collect();

        // Push in randomly sized pieces, taking whatever completes.
        let mut feed = structio::Feed::lines();
        let mut got: Vec<Node> = Vec::new();
        let bytes = text.as_bytes();
        let mut at = 0;
        while at < bytes.len() {
            let take = (1 + r.below(23) as usize).min(bytes.len() - at);
            feed.push(&bytes[at..at + take]);
            at += take;
            while let Some(v) = feed.next_value::<Node>() {
                got.push(v.unwrap_or_else(|e| panic!("iteration {i}: {e} in {text}")));
            }
        }
        feed.end();
        while let Some(v) = feed.next_value::<Node>() {
            got.push(v.unwrap());
        }
        assert_eq!(got, nodes, "iteration {i}");
    }
}

#[test]
fn a_corrupted_stream_never_panics() {
    // Every framing byte is fair game, so the splitter sees unbalanced
    // brackets, stray quotes, and values cut in half.
    let mut r = Rng(0x51ED_1234_ABCD_0004);
    for _ in 0..rounds(2_000) {
        let nodes: Vec<Node> = (0..1 + r.below(3)).map(|_| gen_node(&mut r)).collect();
        let mut bytes = match r.below(3) {
            0 => nodes
                .iter()
                .map(|n| to_string(n) + "\n")
                .collect::<String>(),
            1 => to_string(&nodes),
            _ => nodes.iter().map(to_string).collect::<Vec<_>>().join(" "),
        }
        .into_bytes();
        if bytes.is_empty() {
            continue;
        }
        match r.below(2) {
            0 => {
                let pos = r.below(bytes.len() as u64) as usize;
                bytes[pos] = r.below(128) as u8;
            }
            _ => bytes.truncate(r.below(bytes.len() as u64) as usize),
        }

        let chunk = 1 + r.below(16) as usize;
        for mode in [
            structio::Mode::Lines,
            structio::Mode::Array,
            structio::Mode::Values,
        ] {
            let src = Choppy {
                data: &bytes,
                chunk,
            };
            let mut docs = structio::Documents::new(src, mode)
                .read_size(chunk)
                .max_value(1 << 20);
            for _ in docs.iter::<Node>() {}
        }
    }
}

#[test]
fn corrupted_array_framing_agrees_with_the_batch_parser() {
    // Absence of a panic is a weak property. The strong one is that the
    // streaming side, which only frames, never accepts a document `from_str`
    // rejects: any grammar decision the splitter makes on its own has to match
    // the parser's. A trailing comma is exactly such a decision.
    let mut r = Rng(0x51ED_1234_ABCD_0005);
    for i in 0..rounds(1_500) {
        let nodes: Vec<Node> = (0..r.below(3)).map(|_| gen_node(&mut r)).collect();
        let mut text = to_string(&nodes);
        // Corrupt a structural byte, favouring the ones that decide framing.
        if r.below(2) == 0 {
            let pos = r.below(text.len() as u64) as usize;
            if !text.is_char_boundary(pos) {
                continue;
            }
            let bytes = b"[]{},\": \n1";
            text.insert(pos, bytes[r.below(bytes.len() as u64) as usize] as char);
        } else {
            let cut = r.below(text.len() as u64 + 1) as usize;
            if !text.is_char_boundary(cut) {
                continue;
            }
            text.truncate(cut);
        }

        // An input with no value in it is a stream-level question, not a
        // grammar one: an empty stream has zero values, where `from_str` has
        // a document it cannot produce. Everything else must agree.
        if text.trim().is_empty() {
            continue;
        }
        let batch = from_str::<Vec<Node>>(&text).is_ok();
        let chunk = 1 + r.below(9) as usize;
        let src = Choppy {
            data: text.as_bytes(),
            chunk,
        };
        let mut docs = structio::Documents::array(src).read_size(chunk);
        let streamed = docs.iter::<Node>().all(|v| v.is_ok());
        assert_eq!(
            batch, streamed,
            "iteration {i}: batch={batch} streamed={streamed} for {text:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// BEVE
// ---------------------------------------------------------------------------

#[test]
fn beve_round_trips_are_exact_and_stable() {
    let mut r = Rng(0x5EED_B0DE);
    for _ in 0..rounds(400) {
        let node = gen_node(&mut r);
        let bytes = structio::to_beve(&node);
        // Anything the writer emits has to satisfy the validator, or the two
        // disagree about what the format is.
        structio::validate_beve(&bytes).expect("the writer emitted a valid document");
        let back: Node = structio::from_beve(&bytes).expect("BEVE round trip");
        assert_eq!(back, node);
        // Re-serializing the parsed value must reproduce the same bytes, which
        // is what catches a reader and a writer agreeing only with each other.
        assert_eq!(structio::to_beve(&back), bytes);
    }
}

/// The aligned form is a layout, not a dialect: the same documents, padded so
/// their arrays can be pointed at, and so they must hold the same values and
/// transcode to the same text.
#[test]
fn the_aligned_form_holds_what_the_plain_one_does() {
    let mut r = Rng(0x5EED_A116);
    for _ in 0..rounds(400) {
        let node = gen_node(&mut r);
        let plain = structio::to_beve(&node);
        let aligned = structio::to_beve_aligned(&node);
        structio::validate_beve(&aligned).expect("the writer emitted a valid document");
        let back: Node = structio::from_beve(&aligned).expect("BEVE round trip");
        assert_eq!(back, node);
        assert_eq!(structio::to_beve_aligned(&back), aligned);
        assert_eq!(
            structio::beve_to_json(&aligned).unwrap(),
            structio::beve_to_json(&plain).unwrap()
        );
    }
}

#[test]
fn the_two_formats_agree_on_every_value() {
    // The same struct through both encodings has to land on the same value.
    // Non-finite floats are the one thing JSON cannot carry, and `gen_node`
    // produces them, so compare through BEVE's own round trip on those.
    let mut r = Rng(0x1234_5678);
    for _ in 0..rounds(400) {
        let node = gen_node(&mut r);
        let via_beve: Node = structio::from_beve(&structio::to_beve(&node)).unwrap();
        let via_json: Node = from_str(&to_string(&node)).unwrap();
        let finite = node.numbers.iter().all(|v| v.is_finite())
            && node.leaves.iter().all(|l| l.ratio.is_finite())
            && node.maybe.as_ref().is_none_or(|l| l.ratio.is_finite());
        assert_eq!(via_beve, node);
        if finite {
            assert_eq!(via_json, via_beve);
        }
    }
}

/// A document the validator accepts must transcode, except where JSON has no
/// counterpart at all.
///
/// The two walk the same headers and derive extents from the same code, so a
/// document one takes and the other trips over would mean they had drifted.
/// The exceptions are the values with no JSON form: an extension, and a
/// 128-bit float. Both are well formed, and neither can be written out.
fn transcode_agrees_with_the_validator(bytes: &[u8]) {
    if structio::validate_beve(bytes).is_err() {
        return;
    }
    if let Err(e) = structio::beve_to_json(bytes) {
        assert!(
            // Nothing JSON can hold: a 128-bit float, or an extension that is
            // not a value.
            e.code == ErrorCode::UnsupportedFeature
                // A matrix layout byte outside the two the specification
                // defines. It threatens no extent, so the validator has no
                // reason to look at it; writing it out would mean naming one of
                // the two layouts and transposing the data.
                || e.code == ErrorCode::InvalidMatrixLayout,
            "a valid document failed to transcode with {:?}: {bytes:02x?}",
            e.code
        );
    }
}

#[test]
fn transcoding_produces_what_the_json_writer_would_have() {
    // The transcoder is a third walk over the same headers, and the only cheap
    // way to know it agrees with the other two is to make it produce, without
    // a type, the bytes the typed path produces with one.
    let mut r = Rng(0x7A15_C0DE);
    for _ in 0..rounds(400) {
        let node = gen_node(&mut r);
        let bytes = structio::to_beve(&node);
        let transcoded = structio::beve_to_json(&bytes).expect("transcode");
        assert_eq!(transcoded, to_string(&node));
        // And it has to be JSON, not merely the right text.
        assert_eq!(from_str::<Node>(&transcoded).expect("reparse"), node);
    }
}

/// Laying out text says what writing the value says, under every policy.
fn prettifying_agrees_with_the_writer<O: Options>(node: &Node, compact: &str) {
    let want = to_string_with::<O, _>(node);
    let got = prettify_with::<O>(compact).expect("prettify");
    assert_eq!(
        got, want,
        "laid out differently from written, via {compact}"
    );
    // Laying out what is already laid out moves nothing, and compacting it
    // gets back to the bytes it started from.
    assert_eq!(prettify_with::<O>(&got).expect("prettify"), want);
    assert_eq!(prettify_with::<Standard>(&got).expect("compact"), compact);
}

#[test]
fn prettifying_produces_what_the_json_writer_would_have() {
    // The prettifier is a second walk that emits the same whitespace the value
    // writer emits, without a type to emit it from. The only cheap way to know
    // the two have not drifted is to make them produce the same bytes.
    let mut r = Rng(0x9E37_C0DE_1234);
    for _ in 0..rounds(2_000) {
        let node = gen_node(&mut r);
        let compact = to_string(&node);
        prettifying_agrees_with_the_writer::<Pretty>(&node, &compact);
        prettifying_agrees_with_the_writer::<PrettyInlineArrays>(&node, &compact);
        prettifying_agrees_with_the_writer::<Standard>(&node, &compact);
        // And the layout has to still be JSON, not merely the right text.
        let pretty = prettify(&compact).expect("prettify");
        assert_eq!(from_str::<Node>(&pretty).expect("reparse"), node);
    }
}

/// Whatever the reader accepts, the prettifier accepts and says the same thing
/// about.
///
/// The implication only runs one way: the prettifier has no schema, so it takes
/// documents no `Node` could hold. It must never take *fewer*, and it must
/// never change what a document it took says.
fn prettifying_preserves_whatever_parses(text: &str) {
    let Ok(node) = from_str::<Node>(text) else {
        // Not a `Node`, but still must not panic, and still must round-trip if
        // it was laid out at all.
        if let Ok(pretty) = prettify(text) {
            assert_eq!(prettify(&pretty).expect("prettify"), pretty);
        }
        return;
    };
    let pretty = prettify(text).expect("the reader took it, so this must too");
    assert_eq!(from_str::<Node>(&pretty).expect("reparse"), node);
}

/// Taking a layout away gets back to the writer's compact form, whatever the
/// layout was.
fn minifying_inverts_the_writer<O: Options>(compact: &str) {
    let laid_out = prettify_with::<O>(compact).expect("prettify");
    let got = minify(&laid_out).expect("minify");
    assert_eq!(got, compact, "minified differently from written");
    // The two ways to compact a document must not disagree.
    assert_eq!(prettify_with::<Standard>(&laid_out).expect("compact"), got);
}

#[test]
fn minifying_undoes_every_layout_the_writer_can_produce() {
    let mut r = Rng(0x5EED_D1CE_4321);
    for _ in 0..rounds(2_000) {
        let node = gen_node(&mut r);
        let compact = to_string(&node);
        minifying_inverts_the_writer::<Pretty>(&compact);
        minifying_inverts_the_writer::<PrettyInlineArrays>(&compact);
        // Minifying what is already minified moves nothing. `Standard` as the
        // layout is that same claim, so it is made once rather than per policy.
        assert_eq!(minify(&compact).expect("minify"), compact);
        // And what comes out has to still be JSON, not merely the right text.
        let mini = minify(&prettify(&compact).expect("prettify")).expect("minify");
        assert_eq!(from_str::<Node>(&mini).expect("reparse"), node);
    }
}

/// Can this byte be part of a number or a literal?
///
/// Stated here independently of the crate's own copy, since what these
/// properties are checking is exactly where a token ends.
fn scalar_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'-' | b'+' | b'.')
}

/// Every position in a compact document where a formatter could have put
/// whitespace.
///
/// That is: outside every string, since whitespace inside one belongs to the
/// document, and not between two bytes that would run together into one token
/// if what separated them were taken away.
fn formattable_positions(compact: &str) -> Vec<usize> {
    let b = compact.as_bytes();
    let mut out = Vec::new();
    let (mut in_string, mut escaped) = (false, false);
    for i in 0..=b.len() {
        if !in_string {
            let before = if i > 0 { b[i - 1] } else { b' ' };
            let after = if i < b.len() { b[i] } else { b' ' };
            if !(scalar_byte(before) && scalar_byte(after)) {
                out.push(i);
            }
        }
        if let Some(&c) = b.get(i) {
            if !in_string {
                in_string = c == b'"';
            } else if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
        }
    }
    out
}

/// `compact` with `filler` spliced into every position that can take it.
fn stuff(compact: &str, filler: &dyn Fn(&mut Rng) -> String, r: &mut Rng) -> String {
    let mut out = String::with_capacity(compact.len() * 2);
    let mut last = 0;
    for at in formattable_positions(compact) {
        out.push_str(&compact[last..at]);
        out.push_str(&filler(r));
        last = at;
    }
    out.push_str(&compact[last..]);
    out
}

#[test]
fn whitespace_no_writer_would_have_written_is_still_removed() {
    // Every layout above came out of this crate's own writer, which is the
    // input most likely to work. Real text is spaced by hand, by another
    // library, or not at all: tabs, carriage returns, blank lines, a space
    // before a comma, whitespace around a bracket. All of it has to come back
    // out, and nothing else may move.
    let mut r = Rng(0x5717_FED0_0000_0001);
    let ws = |r: &mut Rng| {
        let n = r.below(4) as usize;
        (0..n)
            .map(|_| [' ', '\t', '\n', '\r'][r.below(4) as usize])
            .collect()
    };
    for _ in 0..rounds(500) {
        let compact = to_string(&gen_node(&mut r));
        let spaced = stuff(&compact, &ws, &mut r);
        assert_eq!(minify(&spaced).expect("minify"), compact);
    }
}

#[test]
fn comments_are_whitespace_wherever_whitespace_can_go() {
    // The only exercise the comment path gets over generated documents, and
    // the only test anywhere of a cached whitespace run that contains one.
    let mut r = Rng(0xC0FF_EE00_C0DE_0002);
    let filler = |r: &mut Rng| match r.below(4) {
        0 => String::new(),
        1 => " /* one */ ".to_string(),
        2 => "\n// to the end\n".to_string(),
        _ => "\t/**/ /*two*/\n".to_string(),
    };
    for _ in 0..rounds(500) {
        let compact = to_string(&gen_node(&mut r));
        let commented = stuff(&compact, &filler, &mut r);
        assert_eq!(
            minify_with::<AllowComments>(&commented).expect("minify"),
            compact
        );
    }
}

/// Whatever the reader accepts, the minifier accepts and says the same thing
/// about.
///
/// The minifier checks less than the prettifier, so this runs even further one
/// way: it takes documents no walk of the structure would. What it must never
/// do is take fewer, or change what a document it took says.
fn minifying_preserves_whatever_parses(text: &str) {
    let Ok(node) = from_str::<Node>(text) else {
        if let Ok(mini) = minify(text) {
            assert!(mini.len() <= text.len(), "minifying grew the document");
            assert_eq!(minify(&mini).expect("minify"), mini);
            // The claim the whole design rests on: taking the layout out is
            // not a repair. Whitespace the reader would have skipped anyway is
            // all that leaves, so a document it refused it must refuse still.
            assert!(
                from_str::<Node>(&mini).is_err(),
                "minifying made an unreadable document readable"
            );
        }
        return;
    };
    let mini = minify(text).expect("the reader took it, so this must too");
    assert_eq!(from_str::<Node>(&mini).expect("reparse"), node);
    // Not compared against `to_string(&node)`: the minifier keeps the input's
    // spelling of a number, and a damaged document can spell one in a way the
    // writer would not have.
}

/// The two ways to compact a document agree, on text neither of them produced.
///
/// Asserted separately from the round trip above because it holds for far more
/// than a `Node`: wherever the structural walk manages to compact something,
/// the scan that does not walk the structure has to reach the same bytes.
fn the_two_compactors_agree(text: &str) {
    if let Ok(walked) = prettify_with::<Standard>(text) {
        let scanned = minify(text).expect("the prettifier took it, so this must too");
        assert_eq!(
            scanned, walked,
            "the minifier and the compact writer differ"
        );
    }
}

/// Every single-byte corruption of a generated document, and every prefix of
/// it, handed to `check`.
///
/// A corruption puts a byte where no writer would have put one; a truncation
/// ends the document somewhere no complete one ever does. Between them they
/// reach the arms a well-formed document never takes.
fn each_damaged_document(seed: u64, laid_out: bool, check: impl Fn(&str)) {
    let mut r = Rng(seed);
    for _ in 0..rounds(2_000) {
        let compact = to_string(&gen_node(&mut r));
        let json = if laid_out {
            prettify(&compact).expect("prettify")
        } else {
            compact
        };
        let mut bytes = json.clone().into_bytes();
        if bytes.is_empty() {
            continue;
        }
        let pos = (r.below(bytes.len() as u64)) as usize;
        // Usually an ASCII byte, since that is where the structure is. Now and
        // then a non-ASCII one, which is the class the word scan has to hold
        // back from matching and which the writers may not carry literally.
        // It goes in as a whole character, because a lone continuation byte
        // would only fail the check below and never be seen.
        if r.below(8) == 0 {
            let c = ['\u{e9}', '\u{20ac}', '\u{1f600}', '\u{80}'][r.below(4) as usize];
            let mut buf = [0u8; 4];
            let wide = c.encode_utf8(&mut buf).as_bytes().to_vec();
            bytes.splice(pos..pos + 1, wide);
        } else {
            bytes[pos] = (r.below(128)) as u8;
        }
        if let Ok(s) = std::str::from_utf8(&bytes) {
            check(s);
        }
        for cut in 0..json.len().min(400) {
            if json.is_char_boundary(cut) {
                check(&json[..cut]);
            }
        }
    }
}

#[test]
fn a_damaged_document_never_defeats_the_minifier() {
    // Over laid-out text, which is what a minifier is given.
    each_damaged_document(0xF1ED_0BAD_1DEA_0001, true, |text| {
        minifying_preserves_whatever_parses(text);
        the_two_compactors_agree(text);
        // The comment path over text nobody wrote on purpose, where a slash
        // may open a comment, sit inside a broken string, or be neither. It
        // takes strictly more than `Standard` does, so all that is asked of it
        // is that whatever it takes, it has already finished with.
        if let Ok(mini) = minify_with::<AllowComments>(text) {
            assert_eq!(minify_with::<AllowComments>(&mini).expect("minify"), mini);
        }
    });
}

#[test]
fn a_damaged_document_never_defeats_the_prettifier() {
    // Over compact text, which is what a prettifier is given.
    each_damaged_document(
        0x0BAD_1DEA_0BAD_1DEA,
        false,
        prettifying_preserves_whatever_parses,
    );
}

/// Reads any value at all, by stepping over it.
///
/// Succeeds exactly when the span the splitter named is one whole value and
/// nothing more, whatever that value happens to be.
#[derive(Default)]
struct Any;

impl<'de> structio::beve::Read<'de> for Any {
    fn read<O: structio::Options>(
        &mut self,
        r: &mut structio::beve::Reader<'de, O>,
    ) -> Result<(), ErrorCode> {
        r.skip_value()
    }
}

/// A document the validator accepts must frame as exactly one value.
///
/// The splitter is a fourth walk over the same headers, and the one that has to
/// suspend part way through them. A document the validator takes and the
/// splitter trips over, or cuts somewhere other than its end, would mean they
/// had drifted.
fn framing_agrees_with_the_validator(bytes: &[u8]) {
    if structio::validate_beve(bytes).is_err() {
        return;
    }
    let mut feed = structio::beve::Feed::values();
    feed.push(bytes);
    feed.end();
    let read = feed.next_value::<Any>();
    assert!(
        read.is_some_and(|r| r.is_ok()),
        "a valid document failed to frame: {bytes:02x?}"
    );
    assert_eq!(
        feed.offset(),
        bytes.len(),
        "a valid document framed short: {bytes:02x?}"
    );
}

#[test]
fn a_streamed_beve_run_recovers_what_the_batch_reader_reads() {
    let mut r = Rng(0x5EED_5EED);
    for _ in 0..rounds(60) {
        let nodes: Vec<Node> = (0..4).map(|_| gen_node(&mut r)).collect();
        let concatenated: Vec<u8> = nodes.iter().flat_map(structio::to_beve).collect();
        let as_array = structio::to_beve(&nodes);

        // Documents back to back, the elements of one array, and both fed a
        // byte at a time, all have to produce the same values.
        for (mode, bytes) in [
            (structio::beve::Mode::Values, &concatenated),
            (structio::beve::Mode::Array, &as_array),
        ] {
            let mut docs = structio::beve::Documents::new(&bytes[..], mode).read_size(3);
            let pulled: Vec<Node> = docs.iter::<Node>().map(Result::unwrap).collect();
            assert_eq!(pulled, nodes, "{mode:?}");

            let mut feed = structio::beve::Feed::new(mode);
            let mut pushed = Vec::new();
            for &b in bytes.iter() {
                feed.push(&[b]);
                while let Some(v) = feed.next_value::<Node>() {
                    pushed.push(v.unwrap());
                }
            }
            feed.end();
            while let Some(v) = feed.next_value::<Node>() {
                pushed.push(v.unwrap());
            }
            assert_eq!(pushed, nodes, "{mode:?}");
        }
    }
}

#[test]
fn beve_reading_into_a_reused_value_matches_a_fresh_one() {
    let mut r = Rng(0xBEEF_F00D);
    let mut reused = Node::default();
    for _ in 0..rounds(300) {
        let node = gen_node(&mut r);
        let bytes = structio::to_beve(&node);
        structio::read_beve_into(&mut reused, &bytes).unwrap();
        let fresh: Node = structio::from_beve(&bytes).unwrap();
        assert_eq!(reused, fresh);
    }
}

#[test]
fn truncating_any_beve_document_never_panics() {
    let mut r = Rng(0x0BAD_CAFE);
    for _ in 0..rounds(60) {
        let bytes = structio::to_beve(&gen_node(&mut r));
        for cut in 0..bytes.len() {
            // Every prefix must be rejected cleanly; a value cannot be
            // complete before its own bytes are.
            assert!(structio::from_beve::<Node>(&bytes[..cut]).is_err(), "{cut}");
            assert!(structio::validate_beve(&bytes[..cut]).is_err(), "{cut}");
            assert!(structio::beve_to_json(&bytes[..cut]).is_err(), "{cut}");
            // A `Feed` rather than a `Documents` because it needs no read
            // buffer, and this loop builds one per prefix.
            let mut feed = structio::beve::Feed::values();
            feed.push(&bytes[..cut]);
            feed.end();
            assert!(feed.next_value::<Any>().is_none_or(|r| r.is_err()), "{cut}");
        }
    }
}

#[test]
fn corrupting_any_beve_byte_never_panics() {
    let mut r = Rng(0xDEAD_10CC);
    for _ in 0..rounds(40) {
        let bytes = structio::to_beve(&gen_node(&mut r));
        for i in 0..bytes.len() {
            for delta in [1u8, 0x0F, 0x55, 0x80, 0xFF] {
                let mut bad = bytes.clone();
                bad[i] = bad[i].wrapping_add(delta);
                // Some corruptions are still valid documents; the only
                // requirement is that none of them panic or hang.
                let _ = structio::from_beve::<Node>(&bad);
                let _ = structio::validate_beve(&bad);
                let _ = structio::from_beve_at::<f64>(&bad, "/numbers/1");
                transcode_agrees_with_the_validator(&bad);
                framing_agrees_with_the_validator(&bad);
            }
        }
    }
}

#[test]
fn arbitrary_bytes_never_panic() {
    let mut r = Rng(0xFACE_B00C);
    for _ in 0..rounds(20_000) {
        let n = r.below(48) as usize;
        let bytes: Vec<u8> = (0..n).map(|_| r.below(256) as u8).collect();
        let _ = structio::from_beve::<Node>(&bytes);
        let _ = structio::from_beve::<Vec<f64>>(&bytes);
        let _ = structio::from_beve::<BTreeMap<String, i32>>(&bytes);
        let _ = structio::from_beve::<Leaf>(&bytes);
        let _ = structio::validate_beve(&bytes);
        transcode_agrees_with_the_validator(&bytes);
        framing_agrees_with_the_validator(&bytes);
        // Seeking walks the same headers on bytes that describe nothing, and
        // each pointer form reaches a different part of that walk.
        for p in ["", "/name", "/0", "/numbers/3", "/lookup/a~1b", "/x/y/z"] {
            let _ = structio::from_beve_at::<Leaf>(&bytes, p);
            let _ = structio::from_beve_at::<f64>(&bytes, p);
        }
    }
}

#[test]
fn every_pointer_finds_what_parsing_the_document_finds() {
    /// Spell a key as a pointer token. The tilde pass runs first, so an escape
    /// the slash pass introduces is not escaped a second time.
    fn token(key: &str) -> String {
        key.replace('~', "~0").replace('/', "~1")
    }

    let mut r = Rng(0x9E37_79B9);
    for _ in 0..rounds(300) {
        let node = gen_node(&mut r);
        let bytes = structio::to_beve(&node);
        let at = |p: &str| -> String { structio::from_beve_at(&bytes, p).unwrap() };

        assert_eq!(at("/name"), node.name);
        // Each container in `Node` reaches the pointer walk a different way: a
        // string array, a numeric block, a generic array of objects, an object
        // whose keys hold the characters that have to be escaped, and two
        // positional structs, one of them stored as a typed array.
        for (i, t) in node.tags.iter().enumerate() {
            assert_eq!(at(&format!("/tags/{i}")), *t);
        }
        for (i, v) in node.numbers.iter().enumerate() {
            assert_eq!(
                structio::from_beve_at::<f64>(&bytes, &format!("/numbers/{i}")).unwrap(),
                *v
            );
        }
        for (i, l) in node.leaves.iter().enumerate() {
            assert_eq!(at(&format!("/leaves/{i}/label")), l.label);
            assert_eq!(
                structio::from_beve_at::<i64>(&bytes, &format!("/leaves/{i}/count")).unwrap(),
                l.count
            );
        }
        for (k, v) in &node.lookup {
            let p = format!("/lookup/{}", token(k));
            assert_eq!(
                structio::from_beve_at::<i32>(&bytes, &p).unwrap(),
                *v,
                "{p:?}"
            );
        }
        for (i, v) in node.fixed.iter().enumerate() {
            assert_eq!(
                structio::from_beve_at::<u16>(&bytes, &format!("/fixed/{i}")).unwrap(),
                *v
            );
        }
        assert_eq!(at("/span/2"), node.span.label);
        assert_eq!(
            structio::from_beve_at::<u8>(&bytes, "/color/1").unwrap(),
            node.color.g
        );

        // One past the end of each is absent, never whatever follows it.
        for p in [
            format!("/tags/{}", node.tags.len()),
            format!("/numbers/{}", node.numbers.len()),
            format!("/leaves/{}", node.leaves.len()),
            "/fixed/4".into(),
            "/color/3".into(),
            "/span/3".into(),
            "/nope".into(),
        ] {
            assert_eq!(
                structio::from_beve_at::<f64>(&bytes, &p).unwrap_err().code,
                ErrorCode::NoSuchValue,
                "{p}"
            );
        }
    }
}

#[test]
fn beve_sink_output_matches_the_in_memory_writer() {
    let mut r = Rng(0xC0DE_D00D);
    for _ in 0..rounds(80) {
        let node = gen_node(&mut r);
        let want = structio::to_beve(&node);
        for cap in [1usize, 2, 3, 7, 64, 4096] {
            let mut got = Vec::new();
            structio::beve::to_writer_buffered(&node, &mut got, cap).unwrap();
            assert_eq!(got, want, "buffer of {cap}");
        }
    }
}
