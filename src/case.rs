//! Compile-time case conversion for keys and variant names.
//!
//! A declaration may name a case rule, and every key it does not spell out
//! explicitly is then converted from the Rust name during const evaluation.
//! `macro_rules!` cannot manipulate a string, but a `const fn` can, and the
//! rest of the machinery already takes a computed key: [`Keys::KEYS`] is an
//! ordinary const expression and [`KeyMap::build`] runs in const context.
//!
//! [`Keys::KEYS`]: crate::Keys::KEYS
//! [`KeyMap::build`]: crate::KeyMap::build
//!
//! # The rule
//!
//! Conversion is not "snake to camel". A name is split into **words** and the
//! words are respelled, which is what lets one rule serve both a field name
//! (`byte_offset`) and a variant name (`ByteOffset`) without being told which
//! it is looking at. A word begins:
//!
//! - after one or more `_`, which are separators and are never emitted;
//! - at a capital that follows a lower-case letter or a digit, so `byteOffset`
//!   splits as `byte` + `Offset`;
//! - at the last capital of a run that is followed by a lower-case letter, so
//!   `HTTPUrl` splits as `HTTP` + `Url` rather than at every capital.
//!
//! Two consequences are worth stating outright, because they are the ones that
//! surprise people:
//!
//! **A leading or trailing `_` is dropped.** In Rust those are the "unused"
//! marker and the keyword escape, so `type_` converts to `type` and `_scratch`
//! to `scratch`. Neither underscore is part of the name the wire knows.
//!
//! **A run of capitals loses its capitals.** `HTTPUrl` under `"camelCase"` is
//! `httpUrl`, not `hTTPUrl` and not `httpURL`, because the rule respells whole
//! words. A format that really wants `httpURL` should say so with an explicit
//! `"httpURL" => http_url`, which is the escape hatch for every name the rule
//! reads differently than you do.
//!
//! A byte above ASCII passes through untouched, since this rule has no case
//! for it, and a non-ASCII identifier therefore keeps its spelling and stays
//! valid UTF-8. It never begins a word on its own account, only a capital and
//! a separator do that, but it does *end* one: `caféBar` splits as
//! `café` + `Bar`, so the `B` keeps its case rather than being run into
//! the word before it.
//!
//! Two names whose converted keys collide are a compile error, from the
//! duplicate check [`KeyMap::build`] already performs.
//!
//! A raw identifier keeps its `r#`, because that is what `stringify!` hands
//! the macro: a field written `r#type` has the key `r#type` with or without a
//! rule, and a rule respells that rather than removing it. Give such a field
//! an explicit key.
//!
//! # Coming from serde
//!
//! The eight spellings are serde's. The rule behind them is not, and three
//! differences will change your bytes:
//!
//! - **`"lowercase"` and `"UPPERCASE"` keep serde's underscores and these do
//!   not.** Serde's field rules take the name to be snake_case already, so
//!   `lowercase` is the identity and `UPPERCASE` is `to_ascii_uppercase`:
//!   `byte_offset` stays `byte_offset` and becomes `BYTE_OFFSET`. Here they
//!   mean what they say, and give `byteoffset` and `BYTEOFFSET`.
//! - **Acronyms in a variant name.** Serde's variant rules break at every
//!   capital, so `HTTPProxy` under `"snake_case"` is `h_t_t_p_proxy`. Here it
//!   is `http_proxy`.
//! - **Serde has two rules and this has one.** Which of serde's applies
//!   depends on whether the name is a field or a variant, so a field that is
//!   not snake_case, or a variant that is not PascalCase, is converted by a
//!   rule that was not written for it. One rule over words has no such seam.
//!
//! Everything else agrees: for a snake_case field and an acronym-free
//! PascalCase variant, the other six rules land on the string serde lands on.

/// How a converted name is spelled.
///
/// The eight rules the declaration macros accept. The spellings are borrowed
/// from `serde`'s `rename_all` so that the vocabulary is familiar, but the
/// *rule* is not serde's and does not always agree with it. See
/// [the module docs](self#coming-from-serde). A declaration names one with
/// [`style`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    /// `"lowercase"`: the words run together, all lower case.
    Lower,
    /// `"UPPERCASE"`: the words run together, all upper case.
    Upper,
    /// `"PascalCase"`: every word capitalized, no separator.
    Pascal,
    /// `"camelCase"`: [`Pascal`](Self::Pascal) with the first word lowered.
    Camel,
    /// `"snake_case"`: lower-case words joined by `_`.
    Snake,
    /// `"SCREAMING_SNAKE_CASE"`: upper-case words joined by `_`.
    ScreamingSnake,
    /// `"kebab-case"`: lower-case words joined by `-`.
    Kebab,
    /// `"SCREAMING-KEBAB-CASE"`: upper-case words joined by `-`.
    ScreamingKebab,
}

/// How one word is spelled, which is the whole of what separates the styles
/// once the words themselves have been found.
#[derive(Clone, Copy)]
enum Word {
    Lower,
    Upper,
    /// First letter up, the rest down.
    Title,
}

impl Style {
    /// The byte between words, if the style has one.
    const fn sep(self) -> Option<u8> {
        match self {
            Style::Snake | Style::ScreamingSnake => Some(b'_'),
            Style::Kebab | Style::ScreamingKebab => Some(b'-'),
            Style::Lower | Style::Upper | Style::Pascal | Style::Camel => None,
        }
    }

    /// How the first word is spelled. It differs from the rest only for
    /// `"camelCase"`, which is the reason this is asked separately at all.
    const fn first(self) -> Word {
        match self {
            Style::Camel => Word::Lower,
            other => other.rest(),
        }
    }

    /// How every word after the first is spelled.
    const fn rest(self) -> Word {
        match self {
            Style::Lower | Style::Snake | Style::Kebab => Word::Lower,
            Style::Upper | Style::ScreamingSnake | Style::ScreamingKebab => Word::Upper,
            Style::Pascal | Style::Camel => Word::Title,
        }
    }
}

/// The [`Style`] a declaration named.
///
/// Looked up during const evaluation rather than by matching the rule against
/// a list of literals in the macro, because a `macro_rules!` matcher cannot
/// see through a fragment: a caller who wraps [`object!`](crate::object) in a
/// macro of their own and forwards the rule as `$rule:literal` would find
/// every spelling rejected. Forwarded through a macro it is still a string
/// by the time it arrives here.
///
/// The declaration itself must write the rule out, since one that is not a
/// literal at the call site is refused before this is reached: a named `const`
/// is not a case rule.
///
/// # Panics
///
/// At compile time, if the rule is not one of the eight.
pub const fn style(rule: &str) -> Style {
    if eq(rule, "lowercase") {
        Style::Lower
    } else if eq(rule, "UPPERCASE") {
        Style::Upper
    } else if eq(rule, "PascalCase") {
        Style::Pascal
    } else if eq(rule, "camelCase") {
        Style::Camel
    } else if eq(rule, "snake_case") {
        Style::Snake
    } else if eq(rule, "SCREAMING_SNAKE_CASE") {
        Style::ScreamingSnake
    } else if eq(rule, "kebab-case") {
        Style::Kebab
    } else if eq(rule, "SCREAMING-KEBAB-CASE") {
        Style::ScreamingKebab
    } else {
        panic!(
            "structio: unrecognized case rule. The rules are \"lowercase\", \"UPPERCASE\", \
             \"PascalCase\", \"camelCase\", \"snake_case\", \"SCREAMING_SNAKE_CASE\", \
             \"kebab-case\" and \"SCREAMING-KEBAB-CASE\", each written as a string"
        )
    }
}

/// String equality, which `==` is not in const context.
const fn eq(a: &str, b: &str) -> bool {
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

/// Whether a byte is the body of a word rather than the start of one: an
/// ASCII lower-case letter, or any byte of a character this rule has no case
/// for.
///
/// A multi-byte character counts because the alternative is worse. Treating it
/// as neither upper nor lower would run it together with a capital beside it,
/// so `caféBar` would convert as one word and lose the `B`'s case, while
/// `café_bar` kept it. It never *begins* a word on its own account, since
/// only a capital does that, but it can end one.
const fn lower_like(b: u8) -> bool {
    b.is_ascii_lowercase() || b >= 0x80
}

/// Whether the byte at `i` begins a word.
///
/// A pure function of the name and the position rather than a flag carried
/// through a walk, so the pass that measures the output and the pass that
/// writes it cannot disagree about where the words are.
///
/// `i` must be the index of a byte that is not `_`.
const fn starts_word(b: &[u8], i: usize) -> bool {
    let mut j = i;
    while j > 0 {
        j -= 1;
        if b[j] == b'_' {
            continue;
        }
        // An underscore stood between them, so the word is already broken.
        if j + 1 != i {
            return true;
        }
        if !b[i].is_ascii_uppercase() {
            return false;
        }
        // `byteOffset`, `vec3X`: a capital after a lower-case letter or a
        // digit opens the next word.
        if lower_like(b[j]) || b[j].is_ascii_digit() {
            return true;
        }
        // `HTTPUrl`: inside a run of capitals only the last one does, and it
        // is the last one exactly when a lower-case letter follows. A digit
        // does not count here, so `HTTP2Client` keeps its `HTTP2` whole.
        if b[j].is_ascii_uppercase() {
            return i + 1 < b.len() && lower_like(b[i + 1]);
        }
        return false;
    }
    // Nothing but separators in front of it: the first word starts here.
    true
}

/// How many words the name holds.
const fn words(b: &[u8]) -> usize {
    let mut i = 0;
    let mut n = 0;
    while i < b.len() {
        if b[i] != b'_' && starts_word(b, i) {
            n += 1;
        }
        i += 1;
    }
    n
}

/// The length of `name` converted to `style`.
///
/// Case is a per-byte operation and separators are one byte, so this is exact
/// rather than an upper bound: it is the `N` [`cased`] is instantiated with.
///
/// # Panics
///
/// At compile time, if the name holds nothing but separators and the
/// conversion would leave an empty key.
pub const fn cased_len(name: &str, style: Style) -> usize {
    let b = name.as_bytes();
    let mut kept = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'_' {
            kept += 1;
        }
        i += 1;
    }
    let words = words(b);
    assert!(
        words > 0,
        "structio: a case rule was applied to a name that is nothing but underscores"
    );
    match style.sep() {
        Some(_) => kept + words - 1,
        None => kept,
    }
}

/// Write `name` converted to `style` into `out`, and answer how many bytes
/// that took. `written` counts words rather than bytes: only the first is
/// spelled differently from the rest, and only by `"camelCase"`.
///
/// The only walk. [`cased_len`] measures the same name by counting rather than
/// by writing, and [`cased`] holds the two against each other, so a name that
/// measured one length and wrote another is a compile error rather than a key
/// with a tail of NULs on it.
const fn write_cased(name: &str, style: Style, out: &mut [u8]) -> usize {
    let b = name.as_bytes();
    let mut i = 0;
    let mut o = 0;
    let mut written = 0usize;
    while i < b.len() {
        if b[i] != b'_' {
            let starts = starts_word(b, i);
            if starts {
                if let Some(sep) = style.sep()
                    && written > 0
                {
                    out[o] = sep;
                    o += 1;
                }
                written += 1;
            }
            let spelling = if written == 1 {
                style.first()
            } else {
                style.rest()
            };
            out[o] = match spelling {
                Word::Lower => b[i].to_ascii_lowercase(),
                Word::Upper => b[i].to_ascii_uppercase(),
                Word::Title => {
                    if starts {
                        b[i].to_ascii_uppercase()
                    } else {
                        b[i].to_ascii_lowercase()
                    }
                }
            };
            o += 1;
        }
        i += 1;
    }
    o
}

/// `name` converted to `style`.
///
/// `N` must be [`cased_len`] of the same name and style; the macro derives it
/// from exactly that call, and a caller that gets it wrong is told so during
/// const evaluation rather than handed a padded key.
pub const fn cased<const N: usize>(name: &str, style: Style) -> [u8; N] {
    let mut out = [0u8; N];
    let n = write_cased(name, style, &mut out);
    assert!(
        n == N,
        "structio: `cased` was given a length that is not the name's `cased_len`"
    );
    out
}

/// The key macros' const `from_utf8`, which is how a computed key reaches the
/// `&'static str` the rest of the crate wants without an `unsafe` block.
///
/// # Panics
///
/// At compile time, if the bytes are not UTF-8. Neither caller can produce
/// that: [`cased`] changes the case of ASCII bytes and copies every other byte
/// through, and [`quoted_key`](crate::json::quoted_key) wraps a key in ASCII
/// punctuation. Both operations preserve UTF-8, and this is what says so
/// without an `unsafe` block.
pub const fn as_str(bytes: &[u8]) -> &str {
    match core::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => panic!("structio: a converted key is not valid UTF-8"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The conversion, from a test rather than from a declaration, so a name
    /// no struct would be caught holding is still pinned.
    /// The conversion, reached from a test rather than from a declaration, so
    /// a name no struct would be caught holding is still pinned.
    fn convert(name: &str, style: Style) -> String {
        // Room for any style: every byte is kept or dropped, and at most one
        // separator goes between two that are kept.
        let mut buf = vec![0u8; 2 * name.len()];
        let n = write_cased(name, style, &mut buf);
        assert_eq!(n, cased_len(name, style), "{name:?} was mismeasured");
        buf.truncate(n);
        String::from_utf8(buf).expect("a converted name is still UTF-8")
    }

    #[test]
    fn words_are_found_at_underscores_and_at_capitals() {
        for (name, want) in [
            ("byte_offset", "byte-offset"),
            ("byteOffset", "byte-offset"),
            ("ByteOffset", "byte-offset"),
            ("HTTPUrl", "http-url"),
            ("HTTP", "http"),
            ("vec3_x", "vec3-x"),
            ("x2Y", "x2-y"),
            ("a_b_c", "a-b-c"),
            ("id", "id"),
            ("A", "a"),
        ] {
            assert_eq!(convert(name, Style::Kebab), want, "{name}");
        }
    }

    #[test]
    fn separators_are_never_emitted() {
        // Leading and trailing underscores are the unused marker and the
        // keyword escape, and runs of them are nothing at all.
        for (name, want) in [
            ("type_", "type"),
            ("_scratch", "scratch"),
            ("__a__b__", "a_b"),
        ] {
            assert_eq!(convert(name, Style::Snake), want, "{name}");
        }
    }

    #[test]
    fn non_ascii_passes_through_and_ends_a_word() {
        assert_eq!(
            convert("caf\u{e9}_au_lait", Style::Camel),
            "caf\u{e9}AuLait"
        );
        assert_eq!(convert("\u{3b1}\u{3b2}", Style::Pascal), "\u{3b1}\u{3b2}");
        // The capital beside it keeps its case, which is the whole reason a
        // byte this rule has no case for still ends a word.
        assert_eq!(convert("caf\u{e9}Bar", Style::Camel), "caf\u{e9}Bar");
        assert_eq!(convert("caf\u{e9}Bar", Style::Kebab), "caf\u{e9}-bar");
        // And it opens one after a run of capitals, exactly as a lower-case
        // letter does: `HTTPUrl` is `HTTP` + `Url`.
        assert_eq!(convert("HTTP\u{e9}lan", Style::Kebab), "htt-p\u{e9}lan");
    }

    #[test]
    fn every_style_spells_one_name_its_own_way() {
        let n = "http_byte_offset";
        assert_eq!(convert(n, Style::Lower), "httpbyteoffset");
        assert_eq!(convert(n, Style::Upper), "HTTPBYTEOFFSET");
        assert_eq!(convert(n, Style::Pascal), "HttpByteOffset");
        assert_eq!(convert(n, Style::Camel), "httpByteOffset");
        assert_eq!(convert(n, Style::Snake), "http_byte_offset");
        assert_eq!(convert(n, Style::ScreamingSnake), "HTTP_BYTE_OFFSET");
        assert_eq!(convert(n, Style::Kebab), "http-byte-offset");
        assert_eq!(convert(n, Style::ScreamingKebab), "HTTP-BYTE-OFFSET");
    }

    #[test]
    fn every_spelling_names_a_style() {
        for (rule, want) in [
            ("lowercase", Style::Lower),
            ("UPPERCASE", Style::Upper),
            ("PascalCase", Style::Pascal),
            ("camelCase", Style::Camel),
            ("snake_case", Style::Snake),
            ("SCREAMING_SNAKE_CASE", Style::ScreamingSnake),
            ("kebab-case", Style::Kebab),
            ("SCREAMING-KEBAB-CASE", Style::ScreamingKebab),
        ] {
            assert_eq!(style(rule), want, "{rule}");
        }
    }
}
