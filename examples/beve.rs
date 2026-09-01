//! One schema, two formats.
//!
//! `cargo run --example beve`

use std::collections::BTreeMap;

#[derive(Default, Debug, PartialEq)]
struct Run {
    label: String,
    /// A typed array: one header, one count, and the slice's own bytes.
    samples: Vec<f64>,
    /// Packed one per bit.
    valid: Vec<bool>,
    /// A string array, with no per-element header.
    channels: Vec<String>,
    /// Integer keys stay integers, rather than being stringified.
    offsets: BTreeMap<u16, i32>,
}
structio::object!(Run {
    label,
    samples,
    valid,
    channels,
    offsets
});

/// Borrowed fields point into the input buffer. `&[u8]` is BEVE only, since
/// JSON has no way to hand back a run of bytes, so this one is declared with
/// `beve_object!` rather than `object!`.
#[derive(Default, Debug, PartialEq)]
struct Frame<'a> {
    id: u32,
    payload: &'a [u8],
    note: &'a str,
}
structio::beve_object!(['de] Frame<'de> { id, payload, note });

fn run() -> Run {
    Run {
        label: "sweep 3".into(),
        samples: (0..1000).map(|i| i as f64 / 7.0).collect(),
        valid: (0..1000).map(|i| i % 5 != 0).collect(),
        channels: vec!["temp".into(), "pressure".into()],
        offsets: BTreeMap::from([(1u16, -20), (2, 40)]),
    }
}

/// Send each value as its own length-prefixed frame.
///
/// A BEVE document states its own extent, so reading one back out of a buffer
/// needs no framing. Sending one over a stream does: the receiver has to know
/// where the value ends before it can parse it.
///
/// Two ways, and the difference is whether the body may be staged in memory
/// first. The second is for a header that has to reach the wire ahead of the
/// bytes it describes.
// docs:begin
fn send_frames(values: &[Run], sink: &mut impl std::io::Write) -> std::io::Result<()> {
    let mut body = Vec::new();
    for value in values {
        // Clears the buffer and keeps its allocation, so after an iteration or
        // two this loop stops allocating altogether.
        structio::write_beve_into(value, &mut body);
        sink.write_all(&(body.len() as u32).to_le_bytes())?;
        sink.write_all(&body)?;
    }
    Ok(())
}

fn stream_frames(values: &[Run], sink: &mut impl std::io::Write) -> std::io::Result<()> {
    for value in values {
        // Exactly what the write below will emit, so the length can go out in
        // front of a body that never exists in memory at all.
        sink.write_all(&(structio::beve_size(value) as u32).to_le_bytes())?;
        structio::to_beve_writer(value, &mut *sink)?;
    }
    Ok(())
}

fn frame_aligned(query: &str, samples: &[f64]) -> Vec<u8> {
    let mut frame = vec![0u8; 8]; // room for the length
    frame.extend_from_slice(query.as_bytes());

    // The body does not begin the document, and the aligned form's padding is
    // chosen from where each payload lands, so both halves are told what stands
    // in front of them. Measured at zero, this length would be wrong.
    let body = structio::beve_size_aligned_after(samples, frame.len());
    frame[..8].copy_from_slice(&(body as u64).to_le_bytes());
    structio::append_beve_aligned(samples, &mut frame);

    frame
}
// docs:end

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let value = run();

    // -- The same value, either way ------------------------------------------
    let text = structio::to_string(&value);
    let binary = structio::to_beve(&value);

    assert_eq!(structio::from_str::<Run>(&text)?, value);
    assert_eq!(structio::from_beve::<Run>(&binary)?, value);

    println!("json  {:>6} bytes", text.len());
    println!(
        "beve  {:>6} bytes  ({:.0}% of the text)",
        binary.len(),
        100.0 * binary.len() as f64 / text.len() as f64
    );
    // The thousand samples are 8 bytes each and nothing more; the thousand
    // booleans are one bit each.
    println!(
        "      {} of those bytes are the sample payload, {} the flags",
        value.samples.len() * 8,
        value.valid.len().div_ceil(8)
    );

    // -- Reading into a value you already have -------------------------------
    // The destination keeps its buffers, so a loop over many documents of the
    // same shape settles into doing no allocation at all.
    let mut into = Run::default();
    structio::read_beve_into(&mut into, &binary)?;
    let at = into.samples.as_ptr();
    structio::read_beve_into(&mut into, &binary)?;
    assert_eq!(into.samples.as_ptr(), at, "the allocation was reused");

    // -- Borrowing straight out of the buffer --------------------------------
    let frame = Frame {
        id: 9,
        payload: &[0xDE, 0xAD, 0xBE, 0xEF],
        note: "no copies here",
    };
    let bytes = structio::to_beve(&frame);
    let back: Frame = structio::from_beve(&bytes)?;
    assert_eq!(back, frame);
    let inside = (back.note.as_ptr() as usize) - (bytes.as_ptr() as usize) < bytes.len();
    println!("borrowed fields point into the input: {inside}");

    // -- Arrays a reader can point at ----------------------------------------
    // BEVE's aligned form pads each numeric payload onto its own element
    // width, so a reader with an aligned buffer can borrow the block where it
    // lies instead of copying it out. The same document otherwise: this one
    // still reads back into the same value.
    assert_eq!(
        structio::from_beve::<Run>(&structio::to_beve_aligned(&value))?,
        value
    );
    let padded = structio::to_beve_aligned(&value.samples);
    let payload = padded.len() - value.samples.len() * 8;
    assert_eq!(payload % 8, 0);
    println!(
        "the aligned form puts {} samples at byte {payload}",
        value.samples.len()
    );

    // And a reader takes that offer up: `try_slice` hands the block back as a
    // `&[f64]` pointing into the document itself. Whether it can is partly the
    // allocator's decision, the document having to sit on an address an `f64`
    // could live at, so the answer is an `Option` rather than a promise, and
    // `Cow<[f64]>` is the field type that copies when it has to.
    let mut reader = structio::beve::Reader::new(&padded);
    match reader.try_slice::<f64>() {
        Some(block) => println!("borrowed {} samples with no copy at all", block.len()),
        None => println!("this buffer is not one a &[f64] can point into"),
    }

    // -- Writing to a sink ---------------------------------------------------
    // Drained as it is produced, so peak memory is the buffer rather than the
    // size of the output.
    let mut file = Vec::new();
    structio::to_beve_writer(&value, &mut file)?;
    assert_eq!(file, binary);

    // -- Length-prefixed frames ----------------------------------------------
    let runs = [run(), run()];
    let mut sink = Vec::new();
    send_frames(&runs, &mut sink)?;

    // The unbuffered form is the same stream, byte for byte, which is the
    // whole claim `beve_size` makes.
    let mut streamed = Vec::new();
    stream_frames(&runs, &mut streamed)?;
    assert_eq!(streamed, sink);

    let mut rest = &sink[..];
    for want in &runs {
        let (len, tail) = rest.split_at(4);
        let len = u32::from_le_bytes(len.try_into()?) as usize;
        let (frame, tail) = tail.split_at(len);
        assert_eq!(&structio::from_beve::<Run>(frame)?, want);
        rest = tail;
    }
    assert!(rest.is_empty());
    println!("read back {} length-prefixed frames", runs.len());

    // A body behind a header, in the aligned form. The payload lands on its
    // element width counted from the start of the frame, whatever the query in
    // front of it is, and the stated length is the body's own.
    for query in ["", "/sensor", "/a/rather/longer/route"] {
        let frame = frame_aligned(query, &value.samples);
        let base = 8 + query.len();
        let stated = u64::from_le_bytes(frame[..8].try_into()?) as usize;
        assert_eq!(frame.len() - base, stated);
        assert_eq!((frame.len() - value.samples.len() * 8) % 8, 0);
        assert_eq!(
            structio::from_beve::<Vec<f64>>(&frame[base..])?,
            value.samples
        );
    }
    println!("aligned bodies stay aligned behind a header of any length");

    // -- Looking at a document you have no type for --------------------------
    // No schema involved: the binary states every value's kind and extent, so
    // the walk that reads it drives the JSON writer directly. What comes out is
    // the same bytes the typed path above produced.
    assert_eq!(structio::beve_to_json(&binary)?, text);
    let unknown = structio::beve_to_json(&structio::to_beve(&frame))?;
    println!("a document with no declared type: {unknown}");

    // -- A file too large to hold --------------------------------------------
    // `from_beve` wants the whole document. `Documents` wants one value of it,
    // so the cost of a million records is one record, not a million. A typed
    // array streams too: the elements carry no headers, and the one the array
    // implied is supplied to the reader with each span.
    let archive = structio::to_beve(&vec![run(), run(), run()]);
    let mut docs = structio::beve::Documents::array(&archive[..]).read_size(16);
    // Into an existing value, so the loop stops allocating after the first
    // record as well as holding only one at a time.
    let mut record = Run::default();
    let mut count = 0;
    while let Some(result) = docs.next_value_into(&mut record) {
        result?;
        count += 1;
    }
    println!(
        "read {count} records out of {} bytes without ever holding the file",
        archive.len()
    );

    // -- What BEVE carries that JSON cannot ----------------------------------
    let odd = vec![f64::NAN, f64::INFINITY, -0.0];
    let back: Vec<f64> = structio::from_beve(&structio::to_beve(&odd))?;
    assert!(back[0].is_nan() && back[1].is_infinite() && back[2].is_sign_negative());
    println!("NaN, infinity, and negative zero survive the round trip");

    // -- Complex numbers and matrices ----------------------------------------
    // BEVE's two data-carrying extensions have types, and both work in JSON as
    // well, so a struct holding one still declares its schema once.
    let signal = vec![
        structio::Complex::new(1.0f64, 2.0),
        structio::Complex::new(3.0, -4.0),
    ];
    let bytes = structio::to_beve(&signal);
    println!(
        "{} complex samples in {} bytes: one header, one count, and the components",
        signal.len(),
        bytes.len()
    );
    assert_eq!(
        structio::from_beve::<Vec<structio::Complex<f64>>>(&bytes)?,
        signal
    );

    // A matrix stores its data as an ordinary value, so a matrix of complex
    // numbers is the run above with a shape in front of it.
    let grid = structio::Matrix::new(structio::MatrixLayout::RowMajor, vec![1, 2], signal.clone())?;
    let bytes = structio::to_beve(&grid);
    assert_eq!(
        structio::from_beve::<structio::Matrix<structio::Complex<f64>>>(&bytes)?,
        grid
    );
    println!("as a matrix: {}", structio::beve_to_json(&bytes)?);

    // -- Fields you do not know about ----------------------------------------
    // A key no field claims is refused by default, which is what catches a
    // typo or the wrong document. `SkipUnknown` asks for the other behaviour:
    // take the members you recognize and step over the rest, so a producer can
    // add fields without breaking you.
    #[derive(Default, Debug, PartialEq)]
    struct JustTheLabel {
        label: String,
    }
    structio::object!(JustTheLabel { label });

    let partial = structio::from_beve_with::<structio::SkipUnknown, JustTheLabel>(&binary)?;
    assert_eq!(partial.label, value.label);
    println!("unknown members are stepped over: {:?}", partial.label);

    Ok(())
}
