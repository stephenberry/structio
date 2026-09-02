//! What a panic out of a `Write` impl leaves in the caller's buffer.
//!
//! Writing cannot fail by returning, so an impl with a value it cannot encode
//! is told to write a documented substitute or panic. That makes an unwind out
//! of `write` part of the contract rather than only a caller's bug, and the
//! question of what the buffer holds afterwards a real one.

use std::panic::{self, AssertUnwindSafe};

use structio::{Options, beve, json};

/// A value that writes some of itself and then gives up.
///
/// Partway rather than immediately, so the buffer under test really is longer
/// than the caller's own bytes at the moment the unwind starts.
struct Boom;

impl json::Write for Boom {
    fn write<O: Options>(&self, w: &mut json::Writer<'_, O>) {
        json::Write::write(&1u32, w);
        panic!("structio test: unencodable");
    }
}

impl beve::Write for Boom {
    fn write<O: Options>(&self, w: &mut beve::Writer<'_, O>) {
        beve::Write::write(&vec![1.5f64, 2.5], w);
        panic!("structio test: unencodable");
    }
}

/// Whether `f` unwound.
///
/// The panic message goes to the harness' capture like any other test output,
/// and is shown only if the test fails.
fn unwound(f: impl FnOnce()) -> bool {
    panic::catch_unwind(AssertUnwindSafe(f)).is_err()
}

/// A header, and room to write behind it without reallocating.
fn framed() -> Vec<u8> {
    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(b"\x00\x01\x02\x03header");
    out
}

#[test]
fn json_append_leaves_the_prefix_alone() {
    let mut out = framed();
    let (before, ptr) = (out.clone(), out.as_ptr());

    assert!(unwound(|| json::append(&Boom, &mut out)));

    assert_eq!(out, before);
    // The same allocation, so the caller keeps the capacity as well as the
    // bytes: the buffer was cut back to the header, not rebuilt.
    assert!(std::ptr::eq(out.as_ptr(), ptr));
}

#[test]
fn beve_append_leaves_the_prefix_alone() {
    let mut out = framed();
    let (before, ptr) = (out.clone(), out.as_ptr());

    assert!(unwound(|| beve::append(&Boom, &mut out)));

    assert_eq!(out, before);
    assert!(std::ptr::eq(out.as_ptr(), ptr));
}

#[test]
fn beve_append_aligned_leaves_the_prefix_alone() {
    let mut out = framed();
    let (before, ptr) = (out.clone(), out.as_ptr());

    assert!(unwound(|| beve::append_aligned(&Boom, &mut out)));

    assert_eq!(out, before);
    assert!(std::ptr::eq(out.as_ptr(), ptr));
}

#[test]
fn an_empty_buffer_is_still_empty() {
    let mut out = Vec::new();

    assert!(unwound(|| json::append(&Boom, &mut out)));

    assert!(out.is_empty());
}

#[test]
fn a_buffer_appended_to_again_carries_on_from_the_header() {
    #[derive(Default)]
    struct Reading {
        id: u32,
    }
    structio::object!(Reading { id });

    let mut out = framed();
    let header = out.len();
    assert!(unwound(|| json::append(&Boom, &mut out)));

    // Nothing of the abandoned document is in the way of the next one.
    json::append(&Reading { id: 7 }, &mut out);
    assert_eq!(&out[header..], br#"{"id":7}"#);
}

#[test]
fn json_write_into_is_left_empty() {
    let mut out = String::from(r#"{"id":1}"#);

    assert!(unwound(|| json::write_into(&Boom, &mut out)));

    // The buffer's contents were this call's to replace, and it took them
    // before it wrote anything. Documented, unlike `append`'s prefix.
    assert!(out.is_empty());
}

#[test]
fn beve_write_into_is_left_empty() {
    let mut out = vec![1u8, 2, 3];

    assert!(unwound(|| beve::write_into(&Boom, &mut out)));

    assert!(out.is_empty());
}
