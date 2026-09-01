//! Compile-time perfect hashing of object keys.
//!
//! This is the Rust analogue of Glaze's `make_keys_info` / `decode_hash`. A
//! [`KeyMap`] is built entirely in `const` context from the key list, so by the
//! time the parser runs, mapping a key to a field index is a load and a mask.
//!
//! The builder walks a ladder of increasingly general (and increasingly
//! expensive) schemes and stops at the first that works:
//!
//! 1. [`HashKind::SingleElement`] - one field, no work at all.
//! 2. `Mod4` / `XorMod4` / `MinusMod4` - three or four fields whose first bytes
//!    happen to map onto `0..4` directly. No table.
//! 3. [`HashKind::UniqueIndexTwo`] - two fields, one byte comparison.
//! 4. [`HashKind::UniqueIndex`] - some byte column differs across every key.
//!    A direct 256-entry lookup, exact, no seed search.
//! 5. `FrontHash2/4/8` - the leading 2, 4, or 8 bytes are distinct.
//! 6. [`HashKind::UniqueIndexSized`] - a byte column plus the key length.
//! 7. [`HashKind::UniquePerLength`] - a byte column chosen per key length.
//! 8. [`HashKind::FullFlat`] - hash of the whole key.
//! 9. [`HashKind::Linear`] - fall back to comparing keys. Always correct.
//!
//! Every scheme is a *candidate* generator. The caller always confirms the hit
//! with a full key comparison, exactly as Glaze's `decode_index` does, so a
//! false positive from an unknown key can never select the wrong field.

use crate::swar::load_u64;

/// Field indices are stored as `u8` with `n` itself as the "no match" sentinel,
/// so a single [`KeyMap`] addresses at most this many keys. Wider objects fall
/// back to [`HashKind::Linear`].
///
/// This is the hard ceiling, not the practical one: with a fixed 256-slot
/// bucket table, random key sets stop finding a perfect hash somewhere around
/// 64 to 80 fields, which is why the whole-key search is not attempted past a
/// width of its own. Regular field names fare much better.
pub const MAX_HASHED_KEYS: usize = 255;

/// Slots in the bucket table. Fixed so that `KeyMap` is a single concrete type
/// (Rust cannot size an associated const's array by an associated const without
/// `generic_const_exprs`). Smaller key sets use only a prefix of it via `mask`,
/// so the cache footprint still scales with the object.
const SLOT_CAP: usize = 256;

/// How a key's bytes are turned into a candidate field index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum HashKind {
    /// No keys.
    Empty,
    /// Exactly one key; the index is always 0.
    SingleElement,
    /// `key[0] % 4`, which lands directly on the field index.
    Mod4,
    /// `(key[0] ^ c0) % 4`.
    XorMod4,
    /// `(key[0] - c0) % 4`.
    MinusMod4,
    /// Two keys distinguished by one byte at `unique_index`.
    UniqueIndexTwo,
    /// `table[key[unique_index]]`, a direct 256-entry map.
    UniqueIndex,
    /// `table[bitmix(key[unique_index] | len << 8, seed) & mask]`.
    UniqueIndexSized,
    /// `table[bitmix(front 2 bytes, seed) & mask]`.
    FrontHash2,
    /// `table[bitmix(front 4 bytes, seed) & mask]`.
    FrontHash4,
    /// `table[rich_bitmix(front 8 bytes, seed) & mask]`.
    FrontHash8,
    /// Like `UniqueIndexSized`, but the byte column is chosen per key length.
    UniquePerLength,
    /// Hash of the entire key.
    FullFlat,
    /// Compare against each key in turn. Correct for any key set.
    Linear,
}

/// A perfect hash over an object's keys, built at compile time.
#[derive(Clone, Copy)]
pub struct KeyMap {
    /// Which scheme `lookup` should run.
    pub kind: HashKind,
    /// Number of keys. Also the "no match" sentinel returned by `lookup`.
    pub n: u32,
    /// Multiplier found by the seed search, for the schemes that need one.
    pub seed: u64,
    /// Byte column that distinguishes the keys, for the `UniqueIndex` schemes.
    pub unique_index: u32,
    /// Shortest key length. Lets the quote scan skip ahead.
    pub min_len: u32,
    /// Longest key length. Rejects over-long keys before hashing.
    pub max_len: u32,
    /// `slots - 1`. Slots is always a power of two, so hashing masks rather
    /// than divides.
    pub mask: u32,
    /// Bucket table mapping a hash to a candidate field index.
    pub table: [u8; SLOT_CAP],
    /// For `UniquePerLength`: the distinguishing byte column for each key
    /// length, indexed by length.
    pub per_len: [u8; SLOT_CAP],
}

// ---------------------------------------------------------------------------
// Hash primitives (ported from Glaze's `bitmix` / `rich_bitmix`)
// ---------------------------------------------------------------------------

#[inline(always)]
pub(crate) const fn bitmix(h: u64, seed: u64) -> u64 {
    let h = h.wrapping_mul(seed);
    h ^ h.rotate_right(49)
}

/// For hashing large, mostly-similar chunks, where `bitmix` alone leaves too
/// much structure in the low bits.
#[inline(always)]
pub(crate) const fn rich_bitmix(h: u64, seed: u64) -> u64 {
    let mut h = h;
    h ^= h >> 23;
    h = h.wrapping_mul(0x2127_599b_f432_5c37);
    h ^= seed;
    h = h.wrapping_mul(0x8803_55f2_1e6d_1965);
    h ^= h >> 47;
    h
}

/// Little-endian load of `n < 8` bytes, zero filled. Const and runtime share
/// this so the table built at compile time matches what the parser computes.
#[inline(always)]
pub(crate) const fn to_u64_below_8(data: &[u8], n: usize) -> u64 {
    let mut v: u64 = 0;
    let mut i = 0;
    while i < n {
        v |= (data[i] as u64) << (8 * i);
        i += 1;
    }
    v
}

#[inline(always)]
pub(crate) const fn to_u64_at(data: &[u8], off: usize) -> u64 {
    (data[off] as u64)
        | ((data[off + 1] as u64) << 8)
        | ((data[off + 2] as u64) << 16)
        | ((data[off + 3] as u64) << 24)
        | ((data[off + 4] as u64) << 32)
        | ((data[off + 5] as u64) << 40)
        | ((data[off + 6] as u64) << 48)
        | ((data[off + 7] as u64) << 56)
}

/// Basis for [`key_digest`]. Odd is the requirement: it is the multiplier in
/// the first [`bitmix`], and multiplication by an odd constant is a bijection,
/// so no part of a chunk is lost. This is the golden-ratio one
/// [`seed_at`] already uses.
const DIGEST_BASIS: u64 = 0x9E37_79B9_7F4A_7C15;

/// The seed-independent fold of a whole key.
///
/// Split out of [`full_hash`] so the perfect-hash search can fold each key once
/// rather than once per key *per seed*. The seed used to enter at the first
/// chunk, which made every attempt re-read every byte of every key.
///
/// Every other scheme in the ladder already had this shape: its hash is
/// `bitmix(something(key), seed)`, with the seed touching only the last step.
/// This gives the whole-key scheme the same shape.
const fn key_digest(data: &[u8], n: usize) -> u64 {
    if n < 8 {
        return to_u64_below_8(data, n);
    }
    let mut h = DIGEST_BASIS;
    let mut i = 0;
    // Consume whole 8-byte chunks, then re-read the final 8 bytes as the tail.
    // Overlapping the tail costs nothing and avoids a partial-load branch.
    while i + 8 <= n {
        h = bitmix(to_u64_at(data, i), h);
        i += 8;
    }
    rich_bitmix(to_u64_at(data, n - 8), h)
}

/// Hash of a whole key: the fold, then the seed. Both the search and the
/// parser go through here, which is what keeps the table and the lookup
/// computing the same value.
pub(crate) const fn full_hash(data: &[u8], n: usize, seed: u64) -> u64 {
    bitmix(key_digest(data, n), seed)
}

/// Deterministic seed sequence for the perfect-hash search. SplitMix64, which
/// gives well-distributed multipliers without a prime table.
const fn seed_at(i: u64) -> u64 {
    let mut z = (i.wrapping_add(1)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// How many seeds to try before giving up on a scheme.
///
/// Typically nowhere near reached: the median is 5 attempts at 32 keys, 16 at
/// 40, 77 at 48. The tail is another matter, and is the reason for the number.
/// At 56 keys, where every key set measured still resolves, one needed 3573.
/// So lowering this would not save the common case anything and would cost the
/// wide-but-solvable case its hash. What keeps the *futile* case from costing
/// seconds is [`MAX_SEARCHED_KEYS`], not a smaller ceiling here.
const SEED_ATTEMPTS: u64 = 4096;

/// Widest object the whole-key search, [`HashKind::FullFlat`], is attempted
/// for.
///
/// The bucket table is a fixed 256 slots, so the birthday bound turns against a
/// perfect hash as the key count climbs. Measured over random key sets: every
/// one resolves at 56 keys, roughly nine in ten at 60, half at 64, one in five
/// at 68, one in fourteen at 72, and it keeps thinning from there rather than
/// stopping. A search that is going to fail still spends the whole of
/// [`SEED_ATTEMPTS`] finding out, seconds of const evaluation per object, and
/// enough such objects in one crate reach `long_running_const_eval`, where
/// rustc refuses the build rather than merely taking its time.
///
/// **This is a trade, not a free win.** Because the odds thin rather than stop,
/// a low single-digit percentage of objects between 73 and about 83 keys lose a
/// hash they would otherwise have found, and read through
/// [`HashKind::Linear`] instead. What they buy is that every object at those
/// widths, the other ninety-odd percent included, compiles at once instead of
/// spending its budget to reach the same fallback, and that a crate full of
/// them still builds.
///
/// It gates this one rung and not the ladder: every scheme above either needs
/// no search at all or is guarded by a predicate that declines the key sets it
/// cannot index, so a wide object whose keys do have a distinguishing column or
/// distinct front bytes still gets a hash, and gets it in a handful of
/// attempts.
const MAX_SEARCHED_KEYS: usize = 72;

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

/// Bucket count for `n` keys: `bit_ceil(n^2) / 2`, matching Glaze, clamped to
/// the fixed table. Quadratic sizing keeps the birthday collision probability
/// low enough that the seed search almost always succeeds on the first few
/// tries.
const fn slots_for(n: usize) -> usize {
    if n <= 1 {
        return 1;
    }
    let sq = n * n;
    let mut s = 1usize;
    while s < sq {
        s *= 2;
    }
    let s = s / 2;
    if s > SLOT_CAP { SLOT_CAP } else { s }
}

impl KeyMap {
    /// Build the map. Called from `const` context by the `object!` macro.
    ///
    /// # Panics
    ///
    /// At compile time, if two keys are equal. A duplicate key is always a bug
    /// in the schema and would make one field permanently unreachable.
    pub const fn build(keys: &[&str]) -> KeyMap {
        let n = keys.len();

        let mut m = KeyMap {
            kind: HashKind::Empty,
            n: n as u32,
            seed: 0,
            unique_index: 0,
            min_len: 0,
            max_len: 0,
            mask: 0,
            table: [0u8; SLOT_CAP],
            per_len: [0u8; SLOT_CAP],
        };

        if n == 0 {
            return m;
        }

        // Duplicate keys would silently shadow a field, so reject them here
        // rather than let one become unreachable at runtime.
        let mut i = 0;
        while i < n {
            let mut j = i + 1;
            while j < n {
                if const_str_eq(keys[i], keys[j]) {
                    panic!("structio: a declaration named the same key or variant twice");
                }
                j += 1;
            }
            i += 1;
        }

        // The parser matches keys against the raw bytes of the document, so a
        // key that JSON would have to escape could never be matched.
        let mut i = 0;
        while i < n {
            let b = keys[i].as_bytes();
            let mut j = 0;
            while j < b.len() {
                if b[j] == b'"' || b[j] == b'\\' || b[j] < 0x20 {
                    panic!("structio: object key contains a character that JSON must escape");
                }
                j += 1;
            }
            i += 1;
        }

        let mut min_len = usize::MAX;
        let mut max_len = 0usize;
        let mut i = 0;
        while i < n {
            let l = keys[i].len();
            if l < min_len {
                min_len = l;
            }
            if l > max_len {
                max_len = l;
            }
            i += 1;
        }
        m.min_len = min_len as u32;
        m.max_len = max_len as u32;

        // Beyond 255 keys the u8 bucket entries run out of sentinel space.
        // Linear search stays correct; objects this wide are vanishingly rare
        // and are dominated by value parsing anyway.
        //
        // Key *length* is deliberately not a reason to fall back here: only
        // `per_len` is indexed by length, and `unique_per_length` declines any
        // key set it cannot index, so one long key no longer costs the whole
        // object its hash.
        if n > MAX_HASHED_KEYS {
            m.kind = HashKind::Linear;
            return m;
        }

        if n == 1 {
            m.kind = HashKind::SingleElement;
            return m;
        }

        m.mask = (slots_for(n) - 1) as u32;

        // --- 2. mod4 family: index falls straight out of the first byte ------
        if (n == 3 || n == 4)
            && min_len > 0
            && let Some(kind) = try_mod4(keys)
        {
            m.kind = kind;
            // `seed` doubles as the reference byte for the xor and subtract
            // variants; `Mod4` ignores it.
            m.seed = keys[0].as_bytes()[0] as u64;
            return m;
        }

        // --- 3/4. a byte column that differs across every key -----------------
        if let Some(uidx) = find_unique_index(keys, min_len) {
            m.unique_index = uidx as u32;
            if n == 2 {
                m.kind = HashKind::UniqueIndexTwo;
                // The byte to compare against; a mismatch means the other key.
                m.seed = keys[0].as_bytes()[uidx] as u64;
                return m;
            }
            m.kind = HashKind::UniqueIndex;
            // Exact by construction: the column is distinct, so no seed search.
            m.table = [n as u8; SLOT_CAP];
            let mut i = 0;
            while i < n {
                m.table[keys[i].as_bytes()[uidx] as usize] = i as u8;
                i += 1;
            }
            return m;
        }

        // --- 5. front-bytes hash ---------------------------------------------
        let slots = slots_for(n);
        let empty_per_len = [0u8; SLOT_CAP];
        let mut width = 2usize;
        while width <= 8 {
            let mode = match width {
                2 => SearchMode::Front2,
                4 => SearchMode::Front4,
                _ => SearchMode::Front8,
            };
            if min_len >= width
                && front_bytes_distinct(keys, width)
                && let Some((seed, table)) = search_seed(keys, slots, mode, 0, &empty_per_len)
            {
                m.kind = match width {
                    2 => HashKind::FrontHash2,
                    4 => HashKind::FrontHash4,
                    _ => HashKind::FrontHash8,
                };
                m.seed = seed;
                m.table = table;
                return m;
            }
            width *= 2;
        }

        // --- 6. byte column combined with the key length ----------------------
        if let Some(uidx) = find_sized_unique_index(keys)
            && let Some((seed, table)) =
                search_seed(keys, slots, SearchMode::Sized, uidx, &empty_per_len)
        {
            m.kind = HashKind::UniqueIndexSized;
            m.unique_index = uidx as u32;
            m.seed = seed;
            m.table = table;
            return m;
        }

        // --- 7. a byte column chosen per key length ---------------------------
        if let Some(per_len) = unique_per_length(keys, min_len, max_len)
            && let Some((seed, table)) =
                search_seed(keys, slots, SearchMode::PerLength, 0, &per_len)
        {
            m.kind = HashKind::UniquePerLength;
            m.seed = seed;
            m.table = table;
            m.per_len = per_len;
            return m;
        }

        // --- 8. hash the whole key --------------------------------------------
        //
        // The one rung no cheap predicate guards, hence the width test. See
        // `MAX_SEARCHED_KEYS`.
        if n <= MAX_SEARCHED_KEYS
            && let Some((seed, table)) =
                search_seed(keys, slots, SearchMode::Full, 0, &empty_per_len)
        {
            m.kind = HashKind::FullFlat;
            m.seed = seed;
            m.table = table;
            return m;
        }

        // --- 9. give up on hashing --------------------------------------------
        m.kind = HashKind::Linear;
        m
    }
}

const fn const_str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// `key[0]` mapped onto `0..4` by one of three cheap operations, landing
/// exactly on the field index so no table is needed.
const fn try_mod4(keys: &[&str]) -> Option<HashKind> {
    let n = keys.len();

    let mut ok = true;
    let mut i = 0;
    while i < n {
        if keys[i].as_bytes()[0] % 4 != i as u8 {
            ok = false;
        }
        i += 1;
    }
    if ok {
        return Some(HashKind::Mod4);
    }

    let c0 = keys[0].as_bytes()[0];

    ok = true;
    i = 0;
    while i < n {
        if (keys[i].as_bytes()[0] ^ c0) % 4 != i as u8 {
            ok = false;
        }
        i += 1;
    }
    if ok {
        return Some(HashKind::XorMod4);
    }

    ok = true;
    i = 0;
    while i < n {
        if keys[i].as_bytes()[0].wrapping_sub(c0) % 4 != i as u8 {
            ok = false;
        }
        i += 1;
    }
    if ok {
        return Some(HashKind::MinusMod4);
    }

    None
}

/// First byte column, within reach of every key, whose values are all distinct.
const fn find_unique_index(keys: &[&str], min_len: usize) -> Option<usize> {
    if min_len == 0 {
        return None;
    }
    let n = keys.len();
    let mut col = 0usize;
    while col < min_len {
        let mut seen = [false; 256];
        let mut distinct = true;
        let mut i = 0;
        while i < n {
            let c = keys[i].as_bytes()[col] as usize;
            if seen[c] {
                distinct = false;
                break;
            }
            seen[c] = true;
            i += 1;
        }
        if distinct {
            return Some(col);
        }
        col += 1;
    }
    None
}

/// Like [`find_unique_index`], but the key length joins the byte, so a column
/// only has to be distinct *within* each length class.
const fn find_sized_unique_index(keys: &[&str]) -> Option<usize> {
    let n = keys.len();
    let mut min_len = usize::MAX;
    let mut i = 0;
    while i < n {
        if keys[i].len() < min_len {
            min_len = keys[i].len();
        }
        i += 1;
    }
    if min_len == 0 {
        return None;
    }

    let mut col = 0usize;
    while col < min_len {
        let mut distinct = true;
        let mut i = 0;
        'outer: while i < n {
            let mut j = i + 1;
            while j < n {
                if keys[i].len() == keys[j].len()
                    && keys[i].as_bytes()[col] == keys[j].as_bytes()[col]
                {
                    distinct = false;
                    break 'outer;
                }
                j += 1;
            }
            i += 1;
        }
        if distinct {
            return Some(col);
        }
        col += 1;
    }
    None
}

const fn front_bytes_distinct(keys: &[&str], width: usize) -> bool {
    let n = keys.len();
    let mut i = 0;
    while i < n {
        let a = to_u64_below_8(keys[i].as_bytes(), width);
        let mut j = i + 1;
        while j < n {
            if a == to_u64_below_8(keys[j].as_bytes(), width) {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

/// One distinguishing byte column per key length, or `None` if some length
/// class has no such column.
const fn unique_per_length(
    keys: &[&str],
    min_len: usize,
    max_len: usize,
) -> Option<[u8; SLOT_CAP]> {
    if min_len == 0 || max_len >= SLOT_CAP {
        return None;
    }
    let n = keys.len();
    let mut out = [255u8; SLOT_CAP];

    let mut len = min_len;
    while len <= max_len {
        // Does any key have this length?
        let mut any = false;
        let mut i = 0;
        while i < n {
            if keys[i].len() == len {
                any = true;
                break;
            }
            i += 1;
        }
        if !any {
            len += 1;
            continue;
        }

        let mut found = false;
        let mut col = 0usize;
        while col < len {
            let mut seen = [false; 256];
            let mut distinct = true;
            let mut i = 0;
            while i < n {
                if keys[i].len() == len {
                    let c = keys[i].as_bytes()[col] as usize;
                    if seen[c] {
                        distinct = false;
                        break;
                    }
                    seen[c] = true;
                }
                i += 1;
            }
            if distinct {
                out[len] = col as u8;
                found = true;
                break;
            }
            col += 1;
        }
        if !found {
            return None;
        }
        len += 1;
    }
    Some(out)
}

// --- seed searches ---------------------------------------------------------
//
// Each returns the first seed that places all `n` keys in distinct buckets,
// together with the resulting table. Unfilled slots hold `n`, the miss
// sentinel, which is what makes an unknown key cheap to reject: it usually
// lands on a sentinel and never reaches a string comparison.

/// Which quantity the seed search feeds into `bitmix`.
///
/// Written as an explicit discriminant rather than a closure parameter because
/// calling a closure from `const fn` is not stable.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchMode {
    Front2,
    Front4,
    Front8,
    Sized,
    PerLength,
    Full,
}

/// The part of a key's hash that does not depend on the seed.
///
/// One per key per search, rather than one per key per attempt. What comes out
/// is exactly what each scheme's lookup feeds to its final mix, so the search
/// and the parser still compute the same hash.
const fn search_preimage(
    key: &[u8],
    mode: SearchMode,
    uidx: usize,
    per_len: &[u8; SLOT_CAP],
) -> u64 {
    match mode {
        SearchMode::Front2 => to_u64_below_8(key, 2),
        SearchMode::Front4 => to_u64_below_8(key, 4),
        SearchMode::Front8 => to_u64_below_8(key, 8),
        SearchMode::Sized => (key[uidx] as u64) | ((key.len() as u64) << 8),
        SearchMode::PerLength => {
            let col = per_len[key.len()] as usize;
            (key[col] as u64) | ((key.len() as u64) << 8)
        }
        SearchMode::Full => key_digest(key, key.len()),
    }
}

/// The seed-dependent step, which is all an attempt has left to do.
///
/// Spelled out rather than given a catch-all arm. A mode added later whose
/// lookup mixes with [`rich_bitmix`] would otherwise default to [`bitmix`]
/// here, and the search would build a table the parser cannot read: not a
/// wrong field, since the candidate is confirmed by comparison, but a field
/// that can never be found. Exhaustive, the compiler asks.
const fn mix_preimage(pre: u64, seed: u64, mode: SearchMode) -> u64 {
    match mode {
        SearchMode::Front8 => rich_bitmix(pre, seed),
        SearchMode::Front2
        | SearchMode::Front4
        | SearchMode::Sized
        | SearchMode::PerLength
        | SearchMode::Full => bitmix(pre, seed),
    }
}

/// Find the first seed that places every key in a distinct bucket.
///
/// Unfilled slots hold `n`, the miss sentinel. That is what makes rejecting an
/// unknown key cheap: it usually lands on a sentinel and never reaches a string
/// comparison at all.
const fn search_seed(
    keys: &[&str],
    slots: usize,
    mode: SearchMode,
    uidx: usize,
    per_len: &[u8; SLOT_CAP],
) -> Option<(u64, [u8; SLOT_CAP])> {
    let n = keys.len();
    let mask = (slots - 1) as u64;

    // Fold every key once. Indexed by key rather than by slot, hence the bound
    // `build` already established.
    let mut pre = [0u64; MAX_HASHED_KEYS];
    let mut i = 0;
    while i < n {
        pre[i] = search_preimage(keys[i].as_bytes(), mode, uidx, per_len);
        i += 1;
    }

    let mut attempt = 0u64;
    while attempt < SEED_ATTEMPTS {
        let seed = seed_at(attempt);
        // A zero seed annihilates `bitmix`, so skip it.
        if seed == 0 {
            attempt += 1;
            continue;
        }
        let mut table = [n as u8; SLOT_CAP];
        let mut ok = true;
        let mut i = 0;
        while i < n {
            let h = mix_preimage(pre[i], seed, mode) & mask;
            if table[h as usize] != n as u8 {
                ok = false;
                break;
            }
            table[h as usize] = i as u8;
            i += 1;
        }
        if ok {
            return Some((seed, table));
        }
        attempt += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Runtime lookup
// ---------------------------------------------------------------------------

/// First `"` at or after `from`, scanned eight bytes at a time.
///
/// Escapes are not interpreted. For a declared key that is irrelevant, since
/// keys never contain escapes, and for an unknown key a wrong answer only
/// produces a wrong *candidate*, which the caller's full comparison rejects.
#[inline(always)]
pub(crate) fn find_quote(buf: &[u8], from: usize) -> Option<usize> {
    const ONES: u64 = 0x0101_0101_0101_0101;
    const HIGH: u64 = 0x8080_8080_8080_8080;
    const QUOTES: u64 = 0x2222_2222_2222_2222;

    let n = buf.len();
    let mut i = from;
    while i + 8 <= n {
        // SAFETY: `i + 8 <= n`, so eight bytes from `i` are in bounds.
        let chunk = u64::from_le(unsafe { (buf.as_ptr().add(i) as *const u64).read_unaligned() });
        let x = chunk ^ QUOTES;
        // Any zero byte in `x` marks a quote: the borrow from the subtract
        // reaches the high bit only where the byte was zero.
        let m = x.wrapping_sub(ONES) & !x & HIGH;
        if m != 0 {
            return Some(i + (m.trailing_zeros() >> 3) as usize);
        }
        i += 8;
    }
    while i < n {
        if buf[i] == b'"' {
            return Some(i);
        }
        i += 1;
    }
    None
}

impl KeyMap {
    /// Map the key starting at `buf[0]` to a candidate field index.
    ///
    /// `buf` runs from the first byte of the key to the end of the document.
    /// Returns `self.n` when there is certainly no match. A returned index is
    /// only a *candidate*: the caller must confirm it with a full comparison,
    /// because unknown keys can collide with occupied buckets.
    #[inline(always)]
    pub fn lookup(&self, keys: &[&'static str], buf: &[u8]) -> usize {
        let n = self.n as usize;
        match self.kind {
            HashKind::Empty => n,
            HashKind::SingleElement => 0,

            HashKind::Mod4 => {
                if buf.is_empty() {
                    return n;
                }
                (buf[0] & 3) as usize
            }
            HashKind::XorMod4 => {
                if buf.is_empty() {
                    return n;
                }
                ((buf[0] ^ self.seed as u8) & 3) as usize
            }
            HashKind::MinusMod4 => {
                if buf.is_empty() {
                    return n;
                }
                (buf[0].wrapping_sub(self.seed as u8) & 3) as usize
            }

            HashKind::UniqueIndexTwo => {
                let u = self.unique_index as usize;
                if buf.len() <= u {
                    return n;
                }
                // One byte decides which of the two it is; no table at all.
                (buf[u] != self.seed as u8) as usize
            }
            HashKind::UniqueIndex => {
                let u = self.unique_index as usize;
                if buf.len() <= u {
                    return n;
                }
                self.table[buf[u] as usize] as usize
            }

            HashKind::FrontHash2 => {
                if buf.len() < 2 {
                    return n;
                }
                // SAFETY: two bytes are in bounds, and the read is unaligned.
                let v =
                    u16::from_le(unsafe { (buf.as_ptr() as *const u16).read_unaligned() }) as u64;
                self.table[(bitmix(v, self.seed) & self.mask as u64) as usize] as usize
            }

            HashKind::FrontHash4 => {
                if buf.len() < 4 {
                    return n;
                }
                // SAFETY: four bytes are in bounds, and the read is unaligned.
                let v =
                    u32::from_le(unsafe { (buf.as_ptr() as *const u32).read_unaligned() }) as u64;
                self.table[(bitmix(v, self.seed) & self.mask as u64) as usize] as usize
            }

            HashKind::FrontHash8 => {
                if buf.len() < 8 {
                    return n;
                }
                // SAFETY: eight bytes are in bounds.
                let v = unsafe { load_u64(buf, 0) };
                let h = rich_bitmix(v, self.seed);
                self.table[(h & self.mask as u64) as usize] as usize
            }

            HashKind::UniqueIndexSized => {
                let len = match self.key_len(buf) {
                    Some(l) => l,
                    None => return n,
                };
                // `unique_index < min_len <= len`, so this byte is inside the key.
                let u = self.unique_index as usize;
                let h = bitmix((buf[u] as u64) | ((len as u64) << 8), self.seed);
                self.table[(h & self.mask as u64) as usize] as usize
            }

            HashKind::UniquePerLength => {
                let len = match self.key_len(buf) {
                    Some(l) => l,
                    None => return n,
                };
                let col = self.per_len[len] as usize;
                if col >= len {
                    // No declared key has this length.
                    return n;
                }
                let h = bitmix((buf[col] as u64) | ((len as u64) << 8), self.seed);
                self.table[(h & self.mask as u64) as usize] as usize
            }

            HashKind::FullFlat => {
                let len = match self.key_len(buf) {
                    Some(l) => l,
                    None => return n,
                };
                let h = full_hash(buf, len, self.seed);
                self.table[(h & self.mask as u64) as usize] as usize
            }

            HashKind::Linear => {
                let len = match self.key_len(buf) {
                    Some(l) => l,
                    None => return n,
                };
                let key = &buf[..len];
                let mut i = 0;
                while i < keys.len() {
                    if keys[i].as_bytes() == key {
                        return i;
                    }
                    i += 1;
                }
                n
            }
        }
    }

    /// Map a key whose length is already known to a candidate field index.
    ///
    /// The same table, seeds, and hash functions as [`KeyMap::lookup`], minus
    /// the quote scan. JSON has to find where a key ends; BEVE states it in a
    /// length prefix, so the schemes that need a length are handed one and the
    /// schemes that do not are unchanged. Both entry points therefore agree on
    /// every key by construction, rather than by two implementations happening
    /// to match.
    ///
    /// A returned index is only a *candidate*: the caller must confirm it with
    /// a full comparison, because unknown keys can collide with occupied
    /// buckets.
    #[inline(always)]
    pub fn lookup_sized(&self, keys: &[&'static str], key: &[u8]) -> usize {
        let n = self.n as usize;
        let len = key.len();
        // A key outside the declared length range cannot be one of ours. This
        // is also what makes the indexed loads below in bounds: every scheme
        // that reads `key[i]` was only chosen because `i < min_len`.
        if len < self.min_len as usize || len > self.max_len as usize {
            return n;
        }
        match self.kind {
            HashKind::Empty => n,
            HashKind::SingleElement => 0,

            HashKind::Mod4 => (key[0] & 3) as usize,
            HashKind::XorMod4 => ((key[0] ^ self.seed as u8) & 3) as usize,
            HashKind::MinusMod4 => (key[0].wrapping_sub(self.seed as u8) & 3) as usize,

            HashKind::UniqueIndexTwo => {
                (key[self.unique_index as usize] != self.seed as u8) as usize
            }
            HashKind::UniqueIndex => self.table[key[self.unique_index as usize] as usize] as usize,

            HashKind::FrontHash2 => {
                // SAFETY: this scheme is only chosen when `min_len >= 2`, and
                // `len >= min_len` was checked above.
                let v =
                    u16::from_le(unsafe { (key.as_ptr() as *const u16).read_unaligned() }) as u64;
                self.table[(bitmix(v, self.seed) & self.mask as u64) as usize] as usize
            }
            HashKind::FrontHash4 => {
                // SAFETY: chosen only when `min_len >= 4`.
                let v =
                    u32::from_le(unsafe { (key.as_ptr() as *const u32).read_unaligned() }) as u64;
                self.table[(bitmix(v, self.seed) & self.mask as u64) as usize] as usize
            }
            HashKind::FrontHash8 => {
                // SAFETY: chosen only when `min_len >= 8`.
                let v = unsafe { load_u64(key, 0) };
                let h = rich_bitmix(v, self.seed);
                self.table[(h & self.mask as u64) as usize] as usize
            }

            HashKind::UniqueIndexSized => {
                let u = self.unique_index as usize;
                let h = bitmix((key[u] as u64) | ((len as u64) << 8), self.seed);
                self.table[(h & self.mask as u64) as usize] as usize
            }

            HashKind::UniquePerLength => {
                // Indexed like `lookup`: this scheme is only chosen when
                // `max_len < SLOT_CAP`, and `len <= max_len` was checked above.
                let col = self.per_len[len] as usize;
                if col >= len {
                    // No declared key has this length.
                    return n;
                }
                let h = bitmix((key[col] as u64) | ((len as u64) << 8), self.seed);
                self.table[(h & self.mask as u64) as usize] as usize
            }

            HashKind::FullFlat => {
                let h = full_hash(key, len, self.seed);
                self.table[(h & self.mask as u64) as usize] as usize
            }

            HashKind::Linear => {
                let mut i = 0;
                while i < keys.len() {
                    if keys[i].as_bytes() == key {
                        return i;
                    }
                    i += 1;
                }
                n
            }
        }
    }

    /// Length of the key at `buf[0]`, if it could be one of ours.
    ///
    /// The scan starts at `min_len`, since a shorter run of bytes cannot be a
    /// declared key, which is where Glaze's `quote_memchr` gets its head start.
    #[inline(always)]
    fn key_len(&self, buf: &[u8]) -> Option<usize> {
        let min = self.min_len as usize;
        if buf.len() < min {
            return None;
        }
        let end = find_quote(buf, min)?;
        if end > self.max_len as usize {
            return None;
        }
        Some(end)
    }
}
