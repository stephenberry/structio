//! Reaching one value inside a BEVE document by JSON Pointer.
//!
//! The property that matters throughout is that seeking to a value and reading
//! it whole must agree: a pointer is a shortcut through the same walk, so any
//! value reachable by parsing the document must come back identical when
//! reached directly, and any value that is not there must say so rather than
//! hand back whatever happened to be at that offset.

use std::collections::{BTreeMap, HashMap};

use structio::beve::header;
use structio::{ErrorCode, beve, from_beve, from_beve_at, read_beve_into_at, to_beve};

#[derive(Default, Debug, PartialEq, Clone)]
struct Server {
    host: String,
    port: u16,
    tags: Vec<String>,
}
structio::object!(Server { host, port, tags });

#[derive(Default, Debug, PartialEq, Clone)]
struct Config {
    name: String,
    servers: Vec<Server>,
    samples: Vec<f64>,
    flags: Vec<bool>,
    counts: Vec<u8>,
    labels: Vec<String>,
    odd: BTreeMap<String, u32>,
}
structio::object!(Config {
    name,
    servers,
    samples,
    flags,
    counts,
    labels,
    odd
});

fn config() -> Config {
    Config {
        name: "prod".into(),
        servers: vec![
            Server {
                host: "a".into(),
                port: 80,
                tags: vec!["edge".into()],
            },
            Server {
                host: "b".into(),
                port: 443,
                tags: vec!["core".into(), "tls".into()],
            },
        ],
        samples: vec![1.5, 2.5, 3.5],
        flags: vec![false, true, false, false, false, false, false, false, true],
        counts: vec![7, 8, 9],
        labels: vec!["zero".into(), "one".into(), "two".into()],
        odd: BTreeMap::from([
            ("a/b".to_string(), 1),
            ("c~d".to_string(), 2),
            (String::new(), 3),
        ]),
    }
}

// ---------------------------------------------------------------------------
// Finding things
// ---------------------------------------------------------------------------

#[test]
fn the_empty_pointer_is_the_whole_document() {
    let bytes = to_beve(&config());
    assert_eq!(from_beve_at::<Config>(&bytes, "").unwrap(), config());
}

#[test]
fn a_pointer_descends_as_far_as_it_is_told() {
    let bytes = to_beve(&config());
    assert_eq!(from_beve_at::<u16>(&bytes, "/servers/1/port").unwrap(), 443);
    assert_eq!(
        from_beve_at::<String>(&bytes, "/servers/1/tags/1").unwrap(),
        "tls"
    );
}

#[test]
fn every_reachable_value_matches_what_parsing_the_whole_document_gives() {
    let c = config();
    let bytes = to_beve(&c);

    assert_eq!(from_beve_at::<String>(&bytes, "/name").unwrap(), c.name);
    assert_eq!(
        from_beve_at::<Vec<f64>>(&bytes, "/samples").unwrap(),
        c.samples
    );
    for (i, s) in c.servers.iter().enumerate() {
        assert_eq!(
            from_beve_at::<Server>(&bytes, &format!("/servers/{i}")).unwrap(),
            *s
        );
        assert_eq!(
            from_beve_at::<String>(&bytes, &format!("/servers/{i}/host")).unwrap(),
            s.host
        );
    }
    for (i, v) in c.samples.iter().enumerate() {
        assert_eq!(
            from_beve_at::<f64>(&bytes, &format!("/samples/{i}")).unwrap(),
            *v
        );
    }
    for (i, v) in c.flags.iter().enumerate() {
        assert_eq!(
            from_beve_at::<bool>(&bytes, &format!("/flags/{i}")).unwrap(),
            *v
        );
    }
    for (i, v) in c.counts.iter().enumerate() {
        assert_eq!(
            from_beve_at::<u8>(&bytes, &format!("/counts/{i}")).unwrap(),
            *v
        );
    }
    for (i, v) in c.labels.iter().enumerate() {
        assert_eq!(
            from_beve_at::<String>(&bytes, &format!("/labels/{i}")).unwrap(),
            *v
        );
    }
}

#[test]
fn an_element_of_a_typed_array_is_found_without_walking_the_ones_before_it() {
    // A million samples, one of which is wanted. Correctness is all that can
    // be asserted here, but the value being right at an index this far in is
    // what the multiply exists for. Miri interprets rather than executes, so
    // it gets a smaller array; the path taken is the same one.
    let n: u64 = if cfg!(miri) { 5_000 } else { 1_000_000 };
    let big: Vec<f64> = (0..n).map(|i| i as f64 * 0.5).collect();
    let bytes = to_beve(&big);
    let last = format!("/{}", n - 1);
    assert_eq!(
        from_beve_at::<f64>(&bytes, &last).unwrap(),
        (n - 1) as f64 * 0.5
    );
    assert_eq!(from_beve_at::<f64>(&bytes, "/0").unwrap(), 0.0);
}

#[test]
fn a_borrowed_value_borrows_out_of_the_document() {
    let bytes = to_beve(&config());
    let host: &str = from_beve_at(&bytes, "/servers/0/host").unwrap();
    assert_eq!(host, "a");
    assert!(bytes.as_ptr_range().contains(&host.as_ptr()));
}

#[test]
fn a_member_this_crate_cannot_decode_is_stepped_over_like_any_other() {
    // Everything off the path is skipped whole, extensions included, which is
    // what lets a document carrying a matrix be reached into for the fields
    // beside it. Object of two members: `a` is a 2x1 matrix, `b` is 1.
    let mut doc = vec![header::OBJECT, 2 << 2];
    doc.extend_from_slice(&[1 << 2, b'a']);
    doc.push(header::header(header::TY_EXTENSION, 0, 0) | (header::EXT_MATRIX << 3));
    doc.push(0); // layout
    doc.extend_from_slice(&[
        header::array_of(header::CAT_UNSIGNED, 0),
        2 << 2,
        2,
        1, // extents
    ]);
    doc.extend_from_slice(&[
        header::array_of(header::CAT_UNSIGNED, 0),
        2 << 2,
        7,
        8, // data
    ]);
    doc.extend_from_slice(&[1 << 2, b'b']);
    doc.extend_from_slice(&[header::number(header::CAT_UNSIGNED, 0), 1]);

    beve::validate(&doc).unwrap();
    assert_eq!(from_beve_at::<u8>(&doc, "/b").unwrap(), 1);
    // Its insides are not addressable, so the extension itself names nothing.
    assert_eq!(
        from_beve_at::<u8>(&doc, "/a/0").unwrap_err().code,
        ErrorCode::NoSuchValue
    );
}

#[test]
fn a_widening_read_works_through_a_pointer_too() {
    // The leniency the ordinary reader has is not lost by arriving another
    // way: element headers are installed exactly as the array driver does.
    let bytes = to_beve(&vec![1u8, 2, 3]);
    assert_eq!(from_beve_at::<u64>(&bytes, "/1").unwrap(), 2);
    assert_eq!(from_beve_at::<f64>(&bytes, "/2").unwrap(), 3.0);
}

// ---------------------------------------------------------------------------
// Escapes and integer keys
// ---------------------------------------------------------------------------

#[test]
fn a_key_holding_a_separator_or_a_tilde_is_spelled_with_an_escape() {
    let bytes = to_beve(&config());
    assert_eq!(from_beve_at::<u32>(&bytes, "/odd/a~1b").unwrap(), 1);
    assert_eq!(from_beve_at::<u32>(&bytes, "/odd/c~0d").unwrap(), 2);
}

#[test]
fn the_empty_key_is_reachable() {
    let bytes = to_beve(&config());
    assert_eq!(from_beve_at::<u32>(&bytes, "/odd/").unwrap(), 3);
}

#[test]
fn an_unescaped_separator_is_a_different_pointer() {
    let bytes = to_beve(&config());
    // `/odd/a/b` looks for a member `a`, not for the member named `a/b`.
    assert_eq!(
        from_beve_at::<u32>(&bytes, "/odd/a/b").unwrap_err().code,
        ErrorCode::NoSuchValue
    );
}

#[test]
fn an_integer_keyed_object_takes_an_integer_token() {
    let map = HashMap::from([(1u32, "one".to_string()), (2, "two".to_string())]);
    let bytes = to_beve(&map);
    assert_eq!(from_beve_at::<String>(&bytes, "/2").unwrap(), "two");
    assert_eq!(
        from_beve_at::<String>(&bytes, "/3").unwrap_err().code,
        ErrorCode::NoSuchValue
    );

    let signed = HashMap::from([(-5i32, 1u8)]);
    let bytes = to_beve(&signed);
    assert_eq!(from_beve_at::<u8>(&bytes, "/-5").unwrap(), 1);
}

#[test]
fn a_non_integer_token_names_no_key_of_an_integer_keyed_object() {
    let bytes = to_beve(&HashMap::from([(1u32, 1u8)]));
    assert_eq!(
        from_beve_at::<u8>(&bytes, "/one").unwrap_err().code,
        ErrorCode::NoSuchValue
    );
}

// ---------------------------------------------------------------------------
// Pointers that name nothing
// ---------------------------------------------------------------------------

#[test]
fn a_missing_key_is_no_such_value() {
    let bytes = to_beve(&config());
    for p in ["/nope", "/servers/0/nope", "/odd/zzz"] {
        assert_eq!(
            from_beve_at::<u8>(&bytes, p).unwrap_err().code,
            ErrorCode::NoSuchValue,
            "{p}"
        );
    }
}

#[test]
fn an_index_past_the_end_is_no_such_value() {
    let bytes = to_beve(&config());
    for p in [
        "/servers/2",
        "/samples/3",
        "/flags/9",
        "/labels/3",
        "/counts/3",
    ] {
        assert_eq!(
            from_beve_at::<u8>(&bytes, p).unwrap_err().code,
            ErrorCode::NoSuchValue,
            "{p}"
        );
    }
}

#[test]
fn descending_into_a_scalar_is_no_such_value() {
    let bytes = to_beve(&config());
    for p in ["/name/0", "/name/x", "/samples/0/0", "/flags/1/0"] {
        assert_eq!(
            from_beve_at::<u8>(&bytes, p).unwrap_err().code,
            ErrorCode::NoSuchValue,
            "{p}"
        );
    }
}

#[test]
fn an_index_too_large_for_any_buffer_is_absent_rather_than_malformed() {
    let bytes = to_beve(&vec![1u8, 2, 3]);
    assert_eq!(
        from_beve_at::<u8>(&bytes, "/99999999999999999999999")
            .unwrap_err()
            .code,
        ErrorCode::NoSuchValue
    );
}

// ---------------------------------------------------------------------------
// Pointers that are not pointers
// ---------------------------------------------------------------------------

#[test]
fn a_pointer_must_begin_with_a_separator() {
    let bytes = to_beve(&config());
    assert_eq!(
        from_beve_at::<String>(&bytes, "name").unwrap_err().code,
        ErrorCode::InvalidPointer
    );
}

#[test]
fn a_stray_tilde_is_a_malformed_pointer_wherever_it_lands() {
    let bytes = to_beve(&config());
    // Reported the same way whether the level it names is an object, an array,
    // or nothing at all, so a typo does not depend on the document to surface.
    for p in ["/od~d", "/servers/~", "/name/~2", "/odd/a~"] {
        assert_eq!(
            from_beve_at::<u8>(&bytes, p).unwrap_err().code,
            ErrorCode::InvalidPointer,
            "{p}"
        );
    }
}

#[test]
fn the_position_after_the_last_element_is_absent_rather_than_malformed() {
    // RFC 6901 defines `-` as the element after the last, so it is a token
    // that is well formed and by construction names nothing. Against an object
    // it is an ordinary key, which is why it cannot be a syntax error: whether
    // a pointer is well formed must not depend on the document.
    let bytes = to_beve(&config());
    assert_eq!(
        from_beve_at::<f64>(&bytes, "/samples/-").unwrap_err().code,
        ErrorCode::NoSuchValue
    );

    let dashed = std::collections::BTreeMap::from([("-".to_string(), 9u8)]);
    assert_eq!(
        from_beve_at::<u8>(&to_beve(&dashed), "/-").unwrap(),
        9,
        "`-` names an ordinary key of an object"
    );
}

#[test]
fn an_array_index_that_is_not_one_is_a_malformed_pointer() {
    let bytes = to_beve(&config());
    // An array has no keys, so there is nothing else these could have meant.
    for p in [
        "/samples/x",
        "/samples/01",
        "/samples/-1",
        "/samples/+1",
        "/samples/",
        "/samples/1.0",
        "/samples/ 1",
    ] {
        assert_eq!(
            from_beve_at::<f64>(&bytes, p).unwrap_err().code,
            ErrorCode::InvalidPointer,
            "{p}"
        );
    }
}

// ---------------------------------------------------------------------------
// What a pointer does and does not require of the document
// ---------------------------------------------------------------------------

#[test]
fn the_bytes_after_the_value_are_never_looked_at() {
    let mut bytes = to_beve(&config());
    let good = bytes.len();
    bytes.extend_from_slice(&[0xff; 8]);

    // Trailing rubbish makes this no longer one document, but the value asked
    // for sits before it and is reached all the same.
    assert_eq!(from_beve_at::<String>(&bytes, "/name").unwrap(), "prod");
    assert_eq!(
        beve::validate(&bytes).unwrap_err().code,
        ErrorCode::TrailingContent
    );
    assert_eq!(from_beve::<Config>(&bytes[..good]).unwrap(), config());
}

#[test]
fn a_truncated_document_fails_rather_than_returning_something() {
    let c = config();
    let bytes = to_beve(&c);
    for n in 0..bytes.len() {
        // A prefix cannot be asserted to fail: one that happens to contain the
        // whole value legitimately finds it, since the bytes after a value are
        // never read. What it must never do is succeed with something else.
        if let Ok(s) = from_beve_at::<Server>(&bytes[..n], "/servers/1") {
            assert_eq!(s, c.servers[1], "prefix of {n}");
        }
        if let Ok(v) = from_beve_at::<f64>(&bytes[..n], "/samples/2") {
            assert_eq!(v, c.samples[2], "prefix of {n}");
        }
        if let Ok(v) = from_beve_at::<bool>(&bytes[..n], "/flags/8") {
            assert_eq!(v, c.flags[8], "prefix of {n}");
        }
    }
    // And the whole document does find each of them, so the loop above is not
    // vacuously satisfied by everything failing.
    assert_eq!(
        from_beve_at::<Server>(&bytes, "/servers/1").unwrap(),
        c.servers[1]
    );
    assert!(from_beve_at::<bool>(&bytes, "/flags/8").unwrap());
}

#[test]
fn a_corrupt_count_cannot_point_the_cursor_outside_the_buffer() {
    // A typed array claiming far more elements than its payload holds.
    let mut bytes = to_beve(&vec![1.0f64, 2.0]);
    bytes[1] = 50 << 2;
    assert_eq!(
        from_beve_at::<f64>(&bytes, "/40").unwrap_err().code,
        ErrorCode::UnexpectedEnd
    );
    assert_eq!(from_beve_at::<f64>(&bytes, "/1").unwrap(), 2.0);
}

#[test]
fn an_aligned_array_is_indexed_like_the_ordinary_form() {
    // Aligned arrays are read but never written, so this one is assembled:
    // header, element header, count, padding length, padding, data.
    let mut bytes = vec![header::ALIGNED_ARRAY];
    bytes.push(header::array_of(header::CAT_UNSIGNED, header::code_for(4)));
    bytes.push(3 << 2);
    bytes.push(2);
    bytes.extend_from_slice(&[0, 0]);
    for v in [10u32, 20, 30] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }

    assert_eq!(from_beve::<Vec<u32>>(&bytes).unwrap(), vec![10, 20, 30]);
    assert_eq!(from_beve_at::<u32>(&bytes, "/2").unwrap(), 30);
    assert_eq!(
        from_beve_at::<u32>(&bytes, "/3").unwrap_err().code,
        ErrorCode::NoSuchValue
    );
    beve::validate(&bytes).unwrap();
}

#[test]
fn an_aligned_array_wrapping_something_other_than_a_typed_array_is_refused() {
    // The width comes from bits the type field does not touch, so a walk that
    // did not check the inner header's *type* would happily compute an extent
    // from a plain number header. All four walks over this form have to agree
    // that it is not one, or validation accepts what reading rejects.
    let mut bytes = vec![header::ALIGNED_ARRAY];
    bytes.push(header::number(header::CAT_UNSIGNED, header::code_for(4)));
    bytes.push(1 << 2);
    bytes.push(0);
    bytes.extend_from_slice(&7u32.to_le_bytes());

    assert_eq!(
        beve::validate(&bytes).unwrap_err().code,
        ErrorCode::InvalidHeader
    );
    assert_eq!(
        from_beve::<Vec<u32>>(&bytes).unwrap_err().code,
        ErrorCode::InvalidHeader
    );
    assert_eq!(
        from_beve_at::<u32>(&bytes, "/0").unwrap_err().code,
        ErrorCode::InvalidHeader
    );
    assert_eq!(
        from_beve::<&[u8]>(&bytes).unwrap_err().code,
        ErrorCode::InvalidHeader
    );
}

// ---------------------------------------------------------------------------
// Reuse
// ---------------------------------------------------------------------------

#[test]
fn reading_at_a_pointer_into_an_existing_value_keeps_its_allocation() {
    let bytes = to_beve(&config());
    let mut server = Server::default();

    read_beve_into_at(&mut server, &bytes, "/servers/1").unwrap();
    assert_eq!(server, config().servers[1]);

    // Grown well past what the value needs, so a reallocation would be visible:
    // a fresh vector filled with the same two elements would not land here.
    server.tags.reserve(64);
    let (capacity, addr) = (server.tags.capacity(), server.tags.as_ptr());

    read_beve_into_at(&mut server, &bytes, "/servers/1").unwrap();
    assert_eq!(server.tags.capacity(), capacity);
    assert_eq!(server.tags.as_ptr(), addr);
    assert_eq!(server, config().servers[1]);
}

#[test]
fn the_reader_can_be_driven_by_hand() {
    let bytes = to_beve(&config());
    let mut r = beve::Reader::new(&bytes);
    r.seek("/servers/0").unwrap();

    let mut server = Server::default();
    r.read(&mut server).unwrap();
    assert_eq!(server, config().servers[0]);
}
