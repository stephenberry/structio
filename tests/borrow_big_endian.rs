//! The borrowing contract that holds on a big-endian target.
//!
//! [`tests/borrow.rs`](borrow.rs) covers the conditions a borrow has to meet
//! and is little-endian only, because on big-endian every one of them is
//! answered before it is asked: a borrow hands back the document's own bytes
//! reinterpreted as numbers, and the payload is little-endian, so
//! `Reader::borrow_block` declines outright.
//!
//! Declining is allowed to cost a copy. It is not allowed to cost an answer,
//! and that is the whole of what this file asserts.
#![cfg(target_endian = "big")]

use std::borrow::Cow;

use structio::{beve_slice_ref, from_beve, to_beve, to_beve_aligned};

#[test]
fn a_block_is_never_borrowed_and_always_read() {
    let samples = vec![1.5f64, -2.25, 3.5, 4.0];

    // The aligned form is the one written so that a borrow *could* happen. On
    // this target it still cannot, in either form.
    for doc in [to_beve(&samples), to_beve_aligned(&samples)] {
        assert!(
            beve_slice_ref::<f64>(&doc).is_none(),
            "borrowed a little-endian block on a big-endian target"
        );
        assert_eq!(from_beve::<Vec<f64>>(&doc).unwrap(), samples);
    }
}

#[test]
fn a_cow_field_takes_the_owned_half() {
    // The field that borrows where it can is the reason any of this exists.
    // Here it always owns, and the values have to survive the copy intact.
    let samples = vec![1.5f64, -2.25, 3.5, 4.0];
    let doc = to_beve_aligned(&samples);

    let read: Cow<'_, [f64]> = from_beve(&doc).unwrap();
    assert!(matches!(read, Cow::Owned(_)), "borrowed on big-endian");
    assert_eq!(read.as_ref(), samples.as_slice());
}
