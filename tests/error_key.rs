//! The key an error carries alongside its offset.
//!
//! `Error::index` answers "where", and for JSON read next to `display_with`
//! that is usually the whole answer: the caret sits under the byte that was
//! wrong. It is a poor answer for a member that is *not there*, whose offset
//! can only be the enclosing object's first byte, and it is a thin answer for
//! BEVE, where there is no text to draw a caret against at all. `Error::key`
//! is the key for those cases.
//!
//! What is asserted here: that it is the key the *document* uses rather than
//! the Rust field, that it is the first absent one in declaration order, that
//! codes with no key to give leave it `None` rather than guessing, that both
//! messages carry it, that it survives the document being dropped, that a
//! discarded read never carries one out with it, and that a hand-written
//! reader can set one of its own.

use structio::{
    Documents, ErrorCode, Matrix, MatrixLayout, RequireKeys, SkipUnknown, beve, from_beve,
    from_beve_with, from_str, from_str_with, json, to_beve, to_string,
};

/// Keys deliberately unlike their fields: one renamed, one converted by a rule.
/// A name reported from `KEYS` is therefore always distinguishable from one
/// reported from `stringify!`.
#[derive(Debug, Default, PartialEq)]
struct Accessor {
    byte_offset: u32,
    component_type: u32,
    #[allow(dead_code)]
    normalized: bool,
}

structio::object!(Accessor as "camelCase" {
    #[required] byte_offset,
    #[required] "type" => component_type,
    normalized,
});

fn accessor_json(members: &str) -> String {
    format!("{{{members}}}")
}

#[test]
fn the_key_is_the_one_the_document_uses_not_the_rust_field() {
    // `byte_offset` under a `camelCase` rule, and `component_type` under an
    // explicit key. Neither Rust spelling may appear.
    let e = from_str::<Accessor>(&accessor_json(r#""type":1"#)).unwrap_err();
    assert_eq!(e.code, ErrorCode::MissingKey);
    assert_eq!(e.key, Some("byteOffset"));

    let e = from_str::<Accessor>(&accessor_json(r#""byteOffset":0"#)).unwrap_err();
    assert_eq!(e.key, Some("type"));
}

#[test]
fn the_first_absent_field_in_declaration_order_is_the_one_named() {
    // Both are missing. The answer is stable, and it is the earlier one.
    let e = from_str::<Accessor>("{}").unwrap_err();
    assert_eq!(e.code, ErrorCode::MissingKey);
    assert_eq!(e.key, Some("byteOffset"));
}

#[test]
fn beve_names_the_field_the_same_way() {
    // The format that needs it most: an offset into a binary document is not
    // something a person can read the document against.
    let doc = to_beve(&Partial { component_type: 1 });
    let e = from_beve::<Accessor>(&doc).unwrap_err();
    assert_eq!(e.code, ErrorCode::MissingKey);
    assert_eq!(e.key, Some("byteOffset"));
}

/// A producer that writes only the second of `Accessor`'s two required keys,
/// so the BEVE document above is one a real writer could have made.
#[derive(Default)]
struct Partial {
    component_type: u32,
}
structio::object!(Partial { "type" => component_type });

#[test]
fn a_whole_policy_requiring_everything_names_a_field_too() {
    #[derive(Debug, Default, PartialEq)]
    struct Loose {
        a: u32,
        b: u32,
    }
    structio::object!(Loose { a, b });

    let e = from_str_with::<RequireKeys, Loose>(r#"{"a":1}"#).unwrap_err();
    assert_eq!(e.code, ErrorCode::MissingKey);
    assert_eq!(e.key, Some("b"));

    let doc = to_beve(&OnlyA { a: 1 });
    let e = from_beve_with::<RequireKeys, Loose>(&doc).unwrap_err();
    assert_eq!(e.key, Some("b"));
}

#[derive(Default)]
struct OnlyA {
    a: u32,
}
structio::object!(OnlyA { a });

#[test]
fn a_code_with_no_key_to_give_carries_none() {
    // The offset already points at the thing that was wrong, so a name here
    // would say the same thing twice. That includes the two codes it would be
    // most tempting to fill in.
    let unknown = r#"{"byteOffset":0,"type":1,"nope":2}"#;
    for e in [
        from_str::<Accessor>(unknown).unwrap_err(),
        from_str::<Accessor>(r#"{"byteOffset":x}"#).unwrap_err(),
        from_str::<Accessor>("[").unwrap_err(),
    ] {
        assert_eq!(e.key, None, "{e:?}");
    }

    // And nothing is lost by leaving it empty for an unknown key: the cursor
    // is already wound back to the key, so the offset points straight at it.
    let e = from_str::<Accessor>(unknown).unwrap_err();
    assert_eq!(e.code, ErrorCode::UnknownKey);
    assert!(unknown[e.index..].starts_with("nope"), "{}", e.index);

    #[derive(Debug, Default, PartialEq)]
    enum Shape {
        #[default]
        Circle,
    }
    structio::unit_enum!(Shape { Circle });

    let e = from_str::<Shape>(r#""Square""#).unwrap_err();
    assert_eq!(e.code, ErrorCode::UnknownVariant);
    assert_eq!(e.key, None);
}

#[test]
fn both_messages_carry_the_key() {
    let text = accessor_json(r#""type":1"#);
    let e = from_str::<Accessor>(&text).unwrap_err();

    let short = e.to_string();
    assert!(short.contains(r#""byteOffset""#), "{short}");
    assert!(short.starts_with("missing object key"), "{short}");

    let long = e.display_with(&text);
    assert!(long.contains(r#""byteOffset""#), "{long}");
    // The two describe the same failure, differing only in how they locate it.
    assert!(
        long.starts_with(r#"missing object key "byteOffset" at line"#),
        "{long}"
    );
    assert!(
        short.starts_with(r#"missing object key "byteOffset" at byte"#),
        "{short}"
    );
}

#[test]
fn an_error_without_a_key_reads_as_it_always_did() {
    // The field is additive: a code with no name renders exactly the string it
    // rendered before there was a field to render.
    let e = from_str::<Accessor>("[").unwrap_err();
    assert_eq!(e.to_string(), "expected '{' at byte 0");
    assert_eq!(
        e.display_with("["),
        "expected '{' at line 1, column 1\n[\n^"
    );
}

#[test]
fn the_key_outlives_the_document() {
    // The reason it is `&'static str`: an `Error` is `Copy`, carries no
    // lifetime, and stays useful after the buffer it indexes is gone. The
    // offset does not survive that, and the name is what is left.
    let e = {
        let owned = accessor_json(r#""type":1"#);
        from_str::<Accessor>(&owned).unwrap_err()
    };
    assert_eq!(e.key, Some("byteOffset"));
    assert!(e.to_string().contains("byteOffset"));
}

/// `StreamError` splits `Io` from `Parse` on the strength of these, so the
/// third field must not have cost either. A compile-time check, because that
/// is what the property is.
const _: fn() = || {
    fn assert<T: Copy + Eq + std::fmt::Debug + 'static>() {}
    assert::<structio::Error>();
};

#[test]
fn the_key_is_part_of_what_makes_two_errors_equal() {
    // Which is the half `Copy + Eq` does not settle: a derive that skipped the
    // field would still compile and would still be `Eq`.
    let named = from_str::<Accessor>("{}").unwrap_err();
    let nameless = structio::Error::new(named.code, named.index);
    assert_eq!(named.code, nameless.code);
    assert_eq!(named.index, nameless.index);
    assert_ne!(named, nameless, "the name has to count");
}

#[test]
fn a_streaming_read_carries_the_key_too() {
    let src = accessor_json(r#""type":1"#);
    let mut docs = Documents::lines(src.as_bytes());
    let e = docs.next_value::<Accessor>().unwrap().unwrap_err();
    let parse = e.as_parse().expect("a parse failure, not i/o");
    assert_eq!(parse.code, ErrorCode::MissingKey);
    assert_eq!(parse.key, Some("byteOffset"));
}

#[test]
fn a_matrix_names_the_member_it_lacks() {
    // The crate's own hand-written reader, which tracks its three keys itself
    // and so has to name them itself.
    let e = from_str_with::<RequireKeys, Matrix<u8>>(r#"{"layout":"row_major"}"#).unwrap_err();
    assert_eq!(e.code, ErrorCode::MissingKey);
    assert_eq!(e.key, Some("extents"));

    let full = Matrix::new(MatrixLayout::RowMajor, vec![2], vec![1u8, 2]).unwrap();
    let object_form = to_string(&full);
    let without_value = object_form.replace(r#","value":[1,2]"#, "");
    let e = from_str_with::<RequireKeys, Matrix<u8>>(&without_value).unwrap_err();
    assert_eq!(e.key, Some("value"));

    // And the same through BEVE's object form, which is the encoding a
    // producer without the matrix extension writes.
    let doc = to_beve(&HalfMatrix {
        layout: "row_major".into(),
        extents: vec![2],
    });
    let e = from_beve_with::<RequireKeys, Matrix<u8>>(&doc).unwrap_err();
    assert_eq!(e.code, ErrorCode::MissingKey);
    assert_eq!(e.key, Some("value"));
}

/// A matrix's object form with its data left out, which the extension form
/// cannot express: an extension carries all three parts by construction.
#[derive(Default)]
struct HalfMatrix {
    layout: String,
    extents: Vec<usize>,
}
structio::object!(HalfMatrix { layout, extents });

#[test]
fn a_hand_written_reader_can_name_its_own_key() {
    // `set_error_key` is the public half of what `read_object` does
    // internally, and the reason it is public: a reader written by hand has
    // the same problem and no other way to solve it.
    #[derive(Debug, Default, PartialEq)]
    struct Pair {
        lo: u32,
        hi: u32,
    }

    impl<'de> json::Read<'de> for Pair {
        fn read<O: structio::Options>(
            &mut self,
            p: &mut json::Parser<'de, O>,
        ) -> Result<(), ErrorCode> {
            let mut seen_hi = false;
            let open = p.position();
            p.read_map(|p, key| match key.as_str() {
                "lo" => self.lo.read(p),
                "hi" => {
                    seen_hi = true;
                    self.hi.read(p)
                }
                _ => p.skip_value(),
            })?;
            if !seen_hi {
                p.rewind(open);
                p.set_error_key("hi");
                return Err(ErrorCode::MissingKey);
            }
            Ok(())
        }
    }

    let e = from_str::<Pair>(r#"{"lo":1}"#).unwrap_err();
    assert_eq!(e.code, ErrorCode::MissingKey);
    assert_eq!(e.key, Some("hi"));
    assert_eq!(e.index, 0, "and located against the object, not its end");
}

#[test]
fn a_key_is_set_only_where_the_read_is_failing() {
    // Nothing clears the field, so the guarantee that a stale name never
    // attaches to a later failure rests on it being written only on a branch
    // that is returning `Err`. A successful read of an object that *could*
    // have failed, followed by a failure that names nothing, is where that
    // would show.
    #[derive(Debug, Default, PartialEq)]
    struct Outer {
        first: Inner,
        second: Inner,
    }
    #[derive(Debug, Default, PartialEq)]
    struct Inner {
        a: u32,
    }
    structio::object!(Outer { first, second });
    structio::object!(Inner {
        #[required]
        a
    });

    // `first` reads cleanly; `second` fails on something with no name.
    let e = from_str::<Outer>(r#"{"first":{"a":1},"second":{"a":x}}"#).unwrap_err();
    assert_eq!(e.code, ErrorCode::ExpectedNumber);
    assert_eq!(e.key, None);

    // And a genuinely missing `a` in the second still names it.
    let e = from_str::<Outer>(r#"{"first":{"a":1},"second":{}}"#).unwrap_err();
    assert_eq!(e.code, ErrorCode::MissingKey);
    assert_eq!(e.key, Some("a"));
}

#[test]
fn a_skipped_unknown_key_leaves_no_key_behind() {
    // `SkipUnknown` steps over a member rather than refusing it, so the reader
    // walks a value it will discard. The failure has to come *after* the skip
    // and have no name of its own, or the missing-key check at the end of the
    // object would overwrite whatever the skip left and the test would pass
    // either way.
    let e = from_str_with::<SkipUnknown, Accessor>(r#"{"nope":{"a":1},"byteOffset":0,"type":x}"#)
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::ExpectedNumber);
    assert_eq!(e.key, None);
}

#[test]
fn a_read_that_is_discarded_carries_no_key_out_of_it() {
    // The one way a stale name could attach to an unrelated error, and the
    // reason `rewind` clears it. A reader that speculates on a *generated*
    // type never sets a name itself: `read_object` sets one behind its back,
    // so a rule saying "clear what you set" would be unfollowable. Winding
    // back is what clears it, which is the operation such a reader already has
    // to perform.
    #[derive(Debug, Default)]
    struct Inner {
        a: u32,
        b: u32,
    }
    structio::object!(Inner {
        #[required]
        a,
        #[required]
        b
    });

    #[derive(Debug, Default)]
    struct Speculative {
        held: u32,
    }

    impl<'de> json::Read<'de> for Speculative {
        fn read<O: structio::Options>(
            &mut self,
            p: &mut json::Parser<'de, O>,
        ) -> Result<(), ErrorCode> {
            let at = p.position();
            let mut probe = Inner::default();
            if json::Read::read(&mut probe, p).is_err() {
                // The whole point: this discards a `MissingKey` naming "b",
                // and has no idea a name was ever set.
                p.rewind(at);
                p.skip_value()?;
                return Err(ErrorCode::ExpectedNumber);
            }
            self.held = probe.a;
            Ok(())
        }
    }

    let e = from_str::<Speculative>(r#"{"a":1}"#).unwrap_err();
    assert_eq!(e.code, ErrorCode::ExpectedNumber);
    assert_eq!(e.key, None, "a discarded read left its key behind");
    assert_eq!(e.to_string(), "expected a number at byte 7");
}

#[test]
fn a_hand_written_beve_reader_can_name_its_own_key_too() {
    #[derive(Debug, Default, PartialEq)]
    struct Pair {
        lo: u32,
        hi: u32,
    }

    impl<'de> beve::Read<'de> for Pair {
        fn read<O: structio::Options>(
            &mut self,
            r: &mut beve::Reader<'de, O>,
        ) -> Result<(), ErrorCode> {
            let mut seen_hi = false;
            let open = r.position();
            r.read_map(|r, key| match key {
                beve::Key::Str("lo") => self.lo.read(r),
                beve::Key::Str("hi") => {
                    seen_hi = true;
                    self.hi.read(r)
                }
                _ => r.skip_value(),
            })?;
            if !seen_hi {
                r.rewind(open);
                r.set_error_key("hi");
                return Err(ErrorCode::MissingKey);
            }
            Ok(())
        }
    }

    let doc = to_beve(&OnlyA { a: 1 });
    let e = from_beve::<Pair>(&doc).unwrap_err();
    assert_eq!(e.code, ErrorCode::MissingKey);
    assert_eq!(e.key, Some("hi"));
    assert_eq!(e.index, 0, "and located against the object, not its end");
}

#[test]
fn a_pointer_read_carries_the_key_of_the_value_it_landed_on() {
    #[derive(Default)]
    struct Wrapper {
        inner: Partial,
    }
    structio::object!(Wrapper { inner });

    let doc = to_beve(&Wrapper {
        inner: Partial { component_type: 1 },
    });
    let e = structio::from_beve_at::<Accessor>(&doc, "/inner").unwrap_err();
    assert_eq!(e.code, ErrorCode::MissingKey);
    assert_eq!(e.key, Some("byteOffset"));
}

#[test]
fn a_second_document_does_not_inherit_the_first_s_key() {
    // Each value gets a fresh parser, so nothing carries across. Asserted
    // rather than assumed, because a reader reused across values would break
    // the whole scheme silently.
    let text = format!(
        "{}\n{{\"byteOffset\":0,\"type\":x}}\n",
        accessor_json(r#""type":1"#)
    );
    let mut docs = Documents::lines(text.as_bytes());

    let first = docs.next_value::<Accessor>().unwrap().unwrap_err();
    let first = first.as_parse().unwrap();
    assert_eq!(
        (first.code, first.key),
        (ErrorCode::MissingKey, Some("byteOffset"))
    );

    let second = docs.next_value::<Accessor>().unwrap().unwrap_err();
    let second = second.as_parse().unwrap();
    assert_eq!((second.code, second.key), (ErrorCode::ExpectedNumber, None));
}
