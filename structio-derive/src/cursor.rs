//! A cursor over a flat run of token trees.
//!
//! `proc_macro` hands a derive its input as a stream, and the shapes this
//! crate has to recognize are shallow: attributes, a name, generics, a body.
//! Walking the trees directly is shorter than describing them to a parser,
//! and it keeps every span on the token it came from.

use proc_macro::{Delimiter, Group, Ident, Punct, Spacing, Span, TokenStream, TokenTree};

use crate::{Error, Result};

pub(crate) struct Cursor {
    tokens: Vec<TokenTree>,
    pos: usize,
    /// Where to point an error that wants a token the stream ran out of.
    end: Span,
}

impl Cursor {
    pub(crate) fn new(stream: TokenStream, end: Span) -> Self {
        Cursor {
            tokens: stream.into_iter().collect(),
            pos: 0,
            end,
        }
    }

    /// A cursor over a group's contents, ending at the group's span.
    pub(crate) fn inside(group: &Group) -> Self {
        Cursor::new(group.stream(), group.span())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    pub(crate) fn peek(&self) -> Option<&TokenTree> {
        self.tokens.get(self.pos)
    }

    pub(crate) fn next(&mut self) -> Option<TokenTree> {
        let tt = self.tokens.get(self.pos).cloned();
        if tt.is_some() {
            self.pos += 1;
        }
        tt
    }

    /// The span of the next token, or of the end when there is none.
    pub(crate) fn span(&self) -> Span {
        self.peek().map_or(self.end, TokenTree::span)
    }

    pub(crate) fn peek_ident(&self, name: &str) -> bool {
        matches!(self.peek(), Some(TokenTree::Ident(i)) if i.to_string() == name)
    }

    pub(crate) fn peek_group(&self, delimiter: Delimiter) -> bool {
        matches!(self.peek(), Some(TokenTree::Group(g)) if g.delimiter() == delimiter)
    }

    pub(crate) fn eat_punct(&mut self, ch: char) -> Option<Punct> {
        match self.peek() {
            Some(TokenTree::Punct(p)) if p.as_char() == ch => {
                let p = p.clone();
                self.pos += 1;
                Some(p)
            }
            _ => None,
        }
    }

    pub(crate) fn eat_ident(&mut self, name: &str) -> Option<Ident> {
        match self.peek() {
            Some(TokenTree::Ident(i)) if i.to_string() == name => {
                let i = i.clone();
                self.pos += 1;
                Some(i)
            }
            _ => None,
        }
    }

    pub(crate) fn eat_group(&mut self, delimiter: Delimiter) -> Option<Group> {
        match self.peek() {
            Some(TokenTree::Group(g)) if g.delimiter() == delimiter => {
                let g = g.clone();
                self.pos += 1;
                Some(g)
            }
            _ => None,
        }
    }

    pub(crate) fn expect_punct(&mut self, ch: char, expected: &str) -> Result<Punct> {
        self.eat_punct(ch)
            .ok_or_else(|| Error::new(self.span(), format!("expected {expected}")))
    }

    pub(crate) fn expect_ident(&mut self, expected: &str) -> Result<Ident> {
        match self.next() {
            Some(TokenTree::Ident(i)) => Ok(i),
            Some(other) => Err(Error::new(other.span(), format!("expected {expected}"))),
            None => Err(Error::new(self.end, format!("expected {expected}"))),
        }
    }

    pub(crate) fn expect_group(&mut self, delimiter: Delimiter, expected: &str) -> Result<Group> {
        self.eat_group(delimiter)
            .ok_or_else(|| Error::new(self.span(), format!("expected {expected}")))
    }

    /// Everything up to the next `,` outside angle brackets, consuming the
    /// comma. A type is what this is for: `HashMap<K, V>` holds a comma the
    /// caller must not stop at.
    pub(crate) fn until_comma(&mut self) -> Vec<TokenTree> {
        let mut out = Vec::new();
        let mut angles = Angles::default();
        while let Some(tt) = self.peek() {
            if angles.at_top() && matches!(tt, TokenTree::Punct(p) if p.as_char() == ',') {
                self.pos += 1;
                break;
            }
            angles.step(tt);
            out.push(tt.clone());
            self.pos += 1;
        }
        out
    }

    /// Everything up to the `>` that closes an already-opened `<`, consuming
    /// it. The tokens between are handed back for splitting.
    pub(crate) fn until_close_angle(&mut self) -> Result<Vec<TokenTree>> {
        let mut out = Vec::new();
        let mut angles = Angles::default();
        while let Some(tt) = self.peek() {
            if angles.at_top() && matches!(tt, TokenTree::Punct(p) if p.as_char() == '>') {
                self.pos += 1;
                return Ok(out);
            }
            angles.step(tt);
            out.push(tt.clone());
            self.pos += 1;
        }
        Err(Error::new(self.end, "expected `>` to close the generics"))
    }
}

/// Angle-bracket depth over a run of tokens. `<` and `>` are not delimiters
/// to `proc_macro`, so a comma inside `Vec<(A, B)>` and one between fields
/// look alike until they are counted. `->` is a `-` joined to a `>`, and is
/// not a close.
#[derive(Default)]
pub(crate) struct Angles {
    depth: u32,
    after_joint_minus: bool,
}

impl Angles {
    pub(crate) fn at_top(&self) -> bool {
        self.depth == 0
    }

    pub(crate) fn step(&mut self, tt: &TokenTree) {
        let arrow_tail = self.after_joint_minus;
        self.after_joint_minus = false;
        if let TokenTree::Punct(p) = tt {
            match p.as_char() {
                '<' => self.depth += 1,
                '>' if !arrow_tail => self.depth = self.depth.saturating_sub(1),
                '-' if p.spacing() == Spacing::Joint => self.after_joint_minus = true,
                _ => {}
            }
        }
    }
}

/// Split a run of tokens at the commas outside angle brackets. A trailing
/// comma yields no empty piece.
pub(crate) fn split_commas(tokens: Vec<TokenTree>) -> Vec<Vec<TokenTree>> {
    let mut pieces = Vec::new();
    let mut current = Vec::new();
    let mut angles = Angles::default();
    for tt in tokens {
        if angles.at_top() && matches!(&tt, TokenTree::Punct(p) if p.as_char() == ',') {
            pieces.push(std::mem::take(&mut current));
            continue;
        }
        angles.step(&tt);
        current.push(tt);
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

/// The position of the first `ch` outside angle brackets, if any.
pub(crate) fn find_punct(tokens: &[TokenTree], ch: char) -> Option<usize> {
    let mut angles = Angles::default();
    for (i, tt) in tokens.iter().enumerate() {
        if angles.at_top() && matches!(tt, TokenTree::Punct(p) if p.as_char() == ch) {
            return Some(i);
        }
        angles.step(tt);
    }
    None
}
