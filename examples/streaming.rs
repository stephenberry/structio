//! The three streaming shapes, end to end.
//!
//! `cargo run --example streaming`

use std::io::Cursor;

use structio::{Documents, Feed, SkipUnknown};

#[derive(Default, Debug, PartialEq)]
struct Record {
    id: u64,
    name: String,
    score: f64,
}
structio::object!(Record { id, name, score });

/// A record that points into the stream buffer instead of copying out of it.
#[derive(Default, Debug)]
struct NameOnly<'a> {
    name: &'a str,
}
structio::object!(['de] NameOnly<'de> { name });

fn records() -> Vec<Record> {
    (1..=4)
        .map(|id| Record {
            id,
            name: format!("record {id}"),
            score: id as f64 / 8.0,
        })
        .collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let all = records();

    // -- Writing ------------------------------------------------------------
    // `to_writer` drains into the sink as the document is produced. Peak
    // memory is the writer's buffer, not the size of the output.
    let mut ndjson = Vec::new();
    for record in &all {
        structio::to_writer(record, &mut ndjson)?;
        ndjson.push(b'\n');
    }
    println!("wrote {} bytes of NDJSON", ndjson.len());

    // -- Reading a sequence -------------------------------------------------
    // `iter` is an ordinary iterator, for values that own their data.
    let mut docs = Documents::lines(Cursor::new(&ndjson));
    let read: Vec<Record> = docs.iter::<Record>().collect::<Result<_, _>>()?;
    assert_eq!(read, all);
    println!("read back {} records", read.len());

    // `next_into` reuses the destination, so a long run stops allocating.
    let mut docs = Documents::lines(Cursor::new(&ndjson));
    let mut scratch = Record::default();
    let mut total = 0.0;
    while let Some(result) = docs.next_value_into(&mut scratch) {
        result?;
        total += scratch.score;
    }
    println!("total score {total}");

    // `next` is the borrowing form: the value points into the stream buffer,
    // and the borrow pins the reader until it is dropped.
    //
    // `NameOnly` claims one of the record's three keys, so this is a partial
    // read and wants the policy that steps over the other two. Without it the
    // default refuses the first key it does not recognize.
    let mut docs = Documents::lines(Cursor::new(&ndjson)).with_options::<SkipUnknown>();
    while let Some(result) = docs.next_value::<NameOnly>() {
        print!("{} ", result?.name);
    }
    println!();

    // -- The elements of one large array ------------------------------------
    let array = structio::to_string(&all);
    let mut docs = Documents::array(Cursor::new(array.as_bytes()));
    let ids: Vec<u64> = docs.iter::<Record>().map(|r| r.unwrap().id).collect();
    println!("array element ids {ids:?}");

    // -- Chunks pushed at you -----------------------------------------------
    // The chunk size divides no value evenly, so most records are interrupted
    // partway through, several of them inside a string.
    let mut feed = Feed::lines();
    let mut got = Vec::new();
    for chunk in ndjson.chunks(7) {
        feed.push(chunk);
        while let Some(result) = feed.next_value::<Record>() {
            got.push(result?);
        }
    }
    feed.end();
    while let Some(result) = feed.next_value::<Record>() {
        got.push(result?);
    }
    assert_eq!(got, all);
    println!("fed {} records in 7-byte chunks", got.len());

    // -- Failures locate themselves against the whole stream ----------------
    let mut broken = ndjson.clone();
    broken.extend_from_slice(b"{\"id\":}\n");
    let mut docs = Documents::lines(Cursor::new(&broken));
    let failure = docs.iter::<Record>().find_map(Result::err).unwrap();
    println!("as expected: {failure}");

    Ok(())
}
