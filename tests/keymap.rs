//! Every hashing scheme in the ladder, checked for exactness.
//!
//! A perfect hash that quietly collides would make one field permanently
//! unreachable, and the failure would look like a missing key rather than a
//! bug, so each scheme is exercised directly.

use structio::KeyMap;
use structio::keymap::HashKind;

/// How many key sets a randomized test should generate.
///
/// Miri interprets rather than executes, at hundreds of times the cost per
/// set, so under it this becomes a sample. The unaligned loads it is here to
/// check are hit by the very first set.
const fn rounds(n: u32) -> u32 {
    if cfg!(miri) { n / 100 + 1 } else { n }
}

/// Look a key up the way the parser does: the cursor sits on the first byte of
/// the key, with the rest of the document still ahead of it.
fn lookup(map: &KeyMap, keys: &[&'static str], key: &str) -> usize {
    let doc = format!("{key}\":1,\"other\":2}}");
    map.lookup(keys, doc.as_bytes())
}

/// Every declared key must map to its own index, and the candidate must
/// survive the full comparison the parser performs afterwards.
///
/// Both entry points are checked together. `lookup` finds the key's end by
/// scanning for a quote, the way JSON has to; `lookup_sized` is handed the
/// length, the way BEVE states it. They share every table and every hash, so
/// they cannot legitimately differ, and this is what pins that.
fn assert_exact(keys: &'static [&'static str]) -> HashKind {
    let map = KeyMap::build(keys);
    assert_eq!(map.n as usize, keys.len());
    for (i, k) in keys.iter().enumerate() {
        assert_eq!(
            lookup(&map, keys, k),
            i,
            "key {k:?} in {keys:?} ({:?}) resolved to the wrong field",
            map.kind
        );
        assert_eq!(
            map.lookup_sized(keys, k.as_bytes()),
            i,
            "sized lookup of {k:?} in {keys:?} ({:?}) resolved to the wrong field",
            map.kind
        );
    }
    map.kind
}

/// Keys that are not ours must either miss outright or land on a candidate
/// whose real key differs, which the parser's comparison then rejects.
fn assert_rejects(keys: &'static [&'static str], strangers: &[&str]) {
    let map = KeyMap::build(keys);
    for s in strangers {
        for i in [lookup(&map, keys, s), map.lookup_sized(keys, s.as_bytes())] {
            if i < keys.len() {
                assert_ne!(keys[i], *s, "{s:?} should not be a declared key");
            }
        }
    }
}

#[test]
fn single_element() {
    assert_eq!(assert_exact(&["only"]), HashKind::SingleElement);
}

#[test]
fn two_elements_use_one_byte() {
    assert_eq!(assert_exact(&["alpha", "beta"]), HashKind::UniqueIndexTwo);
    assert_eq!(assert_exact(&["x", "y"]), HashKind::UniqueIndexTwo);
    assert_rejects(&["alpha", "beta"], &["gamma", "a", "", "alphaa", "bet"]);
}

#[test]
fn mod4_family() {
    // Consecutive first letters are what the subtract variant is for.
    let kind = assert_exact(&["x", "y", "z"]);
    assert!(
        matches!(
            kind,
            HashKind::Mod4 | HashKind::XorMod4 | HashKind::MinusMod4
        ),
        "expected a mod4 scheme, got {kind:?}"
    );
}

#[test]
fn unique_index_is_exact_without_a_seed() {
    let kind = assert_exact(&["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]);
    assert_eq!(kind, HashKind::UniqueIndex);
    assert_rejects(
        &["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"],
        &["golf", "alpha1", "alph", "", "A", "zulu"],
    );
}

#[test]
fn front_hash_handles_shared_first_bytes() {
    // No single byte column separates these, but the leading words do.
    let keys: &[&str] = &["aaaa", "aaab", "aaba", "abaa", "baaa"];
    let kind = assert_exact(keys);
    assert!(
        matches!(
            kind,
            HashKind::FrontHash2 | HashKind::FrontHash4 | HashKind::FrontHash8
        ) || matches!(kind, HashKind::UniqueIndex),
        "got {kind:?}"
    );
    assert_rejects(keys, &["aaac", "bbbb", "aaaaa", "aaa", ""]);
}

#[test]
fn length_and_byte_together() {
    // Same first bytes, different lengths: length has to join the hash.
    let keys: &[&str] = &["a", "aa", "aaa", "aaaa", "aaaaa", "aaaaaa"];
    assert_exact(keys);
    assert_rejects(keys, &["aaaaaaa", "b", "", "ab"]);
}

#[test]
fn long_shared_prefixes_need_the_full_key() {
    let keys: &[&str] = &[
        "configuration_value_alpha",
        "configuration_value_bravo",
        "configuration_value_charlie",
        "configuration_value_delta",
    ];
    assert_exact(keys);
    assert_rejects(
        keys,
        &["configuration_value_echo", "configuration_value_", ""],
    );
}

#[test]
fn realistic_key_sets() {
    let sets: &[&[&'static str]] = &[
        &["id", "name", "email", "created_at", "updated_at"],
        &["x", "y", "z", "w"],
        &[
            "latitude",
            "longitude",
            "altitude",
            "accuracy",
            "heading",
            "speed",
        ],
        &[
            "type",
            "properties",
            "geometry",
            "coordinates",
            "features",
            "bbox",
        ],
        &["a"],
        &["", "b"],
        &["_", "__", "___"],
        &["Ünïcödé", "ключ", "键"],
        &["with space", "with-dash", "with.dot", "with/slash"],
    ];
    for keys in sets {
        assert_exact(keys);
    }
}

/// Wide objects, where the seed search has the least room and the fallback
/// matters most.
#[test]
fn wide_objects() {
    macro_rules! keys64 {
        () => {
            &[
                "f00", "f01", "f02", "f03", "f04", "f05", "f06", "f07", "f08", "f09", "f10", "f11",
                "f12", "f13", "f14", "f15", "f16", "f17", "f18", "f19", "f20", "f21", "f22", "f23",
                "f24", "f25", "f26", "f27", "f28", "f29", "f30", "f31", "f32", "f33", "f34", "f35",
                "f36", "f37", "f38", "f39", "f40", "f41", "f42", "f43", "f44", "f45", "f46", "f47",
                "f48", "f49", "f50", "f51", "f52", "f53", "f54", "f55", "f56", "f57", "f58", "f59",
                "f60", "f61", "f62", "f63",
            ]
        };
    }
    let keys: &[&'static str] = keys64!();
    assert_exact(keys);
    assert_rejects(keys, &["f64", "f99", "g00", "f0", ""]);
}

/// Generated key sets, to cover shapes nobody thought to write down.
#[test]
fn generated_key_sets_are_all_exact() {
    // A deterministic generator, so a failure is reproducible.
    let mut state: u64 = 0xDEAD_BEEF_CAFE_F00D;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz_0123456789";

    for _ in 0..rounds(400) {
        let n = 1 + (next() % 40) as usize;
        let mut owned: Vec<String> = Vec::with_capacity(n);
        while owned.len() < n {
            let len = 1 + (next() % 12) as usize;
            let mut s = String::with_capacity(len);
            for _ in 0..len {
                s.push(ALPHABET[(next() % ALPHABET.len() as u64) as usize] as char);
            }
            if !owned.contains(&s) {
                owned.push(s);
            }
        }
        // `KeyMap::build` takes `&'static str` in practice; leaking here keeps
        // the test honest about the lifetime without contorting the API.
        let keys: &'static [&'static str] = Box::leak(
            owned
                .iter()
                .map(|s| &*Box::leak(s.clone().into_boxed_str()))
                .collect::<Vec<&'static str>>()
                .into_boxed_slice(),
        );

        let map = KeyMap::build(keys);
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(
                lookup(&map, keys, k),
                i,
                "{:?} mis-resolved {k:?} among {} keys",
                map.kind,
                keys.len()
            );
            assert_eq!(
                map.lookup_sized(keys, k.as_bytes()),
                i,
                "{:?} sized-mis-resolved {k:?} among {} keys",
                map.kind,
                keys.len()
            );
        }
        // Keys of every other length must not be claimed by a field they are
        // not. The generator's alphabet is lowercase, so none of these can
        // collide with a declared key by being one.
        for stranger in ["", "X", "ZZ", "ABCDEFGHIJKL", &"Q".repeat(40)] {
            let i = map.lookup_sized(keys, stranger.as_bytes());
            if i < keys.len() {
                assert_ne!(keys[i], stranger, "{stranger:?} is not a declared key");
            }
        }
    }
}
