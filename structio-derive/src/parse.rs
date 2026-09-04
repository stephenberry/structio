//! From the derive's input to the shape of the type.
//!
//! Only what the declaration macros need is kept: the name, the generics with
//! their bounds, each field's name, each variant's name and how many values
//! it carries, and the `#[structio(..)]` attributes on all of them. Field
//! types are stepped over, since the macros never see them either: they read
//! a field through `self.name` and let the compiler find the type.

use proc_macro::{Delimiter, Ident, Literal, Span, TokenStream, TokenTree};

use crate::cursor::{Cursor, find_punct, split_commas};
use crate::{Error, Result};

pub(crate) struct Input {
    pub(crate) attrs: Vec<Meta>,
    pub(crate) name: Ident,
    pub(crate) generics: Vec<Param>,
    pub(crate) shape: Shape,
}

/// One generic parameter, with the bounds it was declared with and any a
/// `where` clause added.
pub(crate) enum Param {
    /// The two tokens of `'a`, then the bounds after its `:`.
    Lifetime {
        name: [TokenTree; 2],
        bounds: Vec<TokenTree>,
    },
    Type {
        name: Ident,
        bounds: Vec<TokenTree>,
    },
    /// `const N: usize`, kept verbatim for the impl generics.
    Const {
        name: Ident,
        decl: Vec<TokenTree>,
    },
}

pub(crate) enum Shape {
    Struct(Vec<Field>),
    Enum(Vec<Variant>),
}

pub(crate) struct Field {
    pub(crate) attrs: Vec<Meta>,
    pub(crate) name: Ident,
}

pub(crate) struct Variant {
    pub(crate) attrs: Vec<Meta>,
    pub(crate) name: Ident,
    pub(crate) payload: Payload,
}

pub(crate) enum Payload {
    Unit,
    /// `Variant(T)`, the one shape the enum macros take a value in.
    One,
    /// `Variant(A, B)`, spanned at the parentheses.
    Many(Span),
    /// `Variant { a: A }`, spanned at the braces.
    Named(Span),
}

/// One entry of a `#[structio(..)]` list: `name` or `name = "value"`.
pub(crate) struct Meta {
    pub(crate) name: Ident,
    pub(crate) value: Option<Literal>,
}

pub(crate) fn parse(input: TokenStream) -> Result<Input> {
    let mut c = Cursor::new(input, Span::call_site());
    let attrs = attributes(&mut c)?;
    visibility(&mut c);

    let keyword = c.expect_ident("`struct` or `enum`")?;
    let is_enum = match keyword.to_string().as_str() {
        "struct" => false,
        "enum" => true,
        "union" => {
            return Err(Error::new(
                keyword.span(),
                "a union has no schema: which field holds the value is not \
                 something the bytes can say. Derive on a struct or an enum.",
            ));
        }
        _ => return Err(Error::new(keyword.span(), "expected `struct` or `enum`")),
    };
    let name = c.expect_ident("the type's name")?;

    let mut generics = if c.eat_punct('<').is_some() {
        params(c.until_close_angle()?)?
    } else {
        Vec::new()
    };

    if !is_enum && !c.peek_group(Delimiter::Brace) && !c.peek_ident("where") {
        let what = if c.peek_group(Delimiter::Parenthesis) {
            "a tuple struct has no field names to be keys, and the positional \
             macro counts by name too. Give the fields names, or declare a \
             tuple instead of a struct"
        } else {
            "a unit struct has no fields to put on the wire"
        };
        return Err(Error::new(c.span(), what));
    }

    if let Some(kw) = c.eat_ident("where") {
        where_clause(&mut c, &mut generics, kw.span())?;
    }

    let body = c.expect_group(Delimiter::Brace, "the type's body")?;
    let shape = if is_enum {
        Shape::Enum(variants(&body)?)
    } else {
        Shape::Struct(fields(&body)?)
    };

    Ok(Input {
        attrs,
        name,
        generics,
        shape,
    })
}

/// The `#[structio(..)]` attributes in a run of outer attributes, in order.
/// Every other attribute is stepped over.
fn attributes(c: &mut Cursor) -> Result<Vec<Meta>> {
    let mut metas = Vec::new();
    while c.eat_punct('#').is_some() {
        let group = c.expect_group(Delimiter::Bracket, "an attribute")?;
        let mut inner = Cursor::inside(&group);
        if inner.eat_ident("structio").is_none() {
            continue;
        }
        match inner.eat_group(Delimiter::Parenthesis) {
            Some(list) if inner.is_empty() => metas.extend(meta_list(&list)?),
            _ => {
                return Err(Error::new(
                    group.span(),
                    "`#[structio]` takes a parenthesized list: \
                     `#[structio(rename = \"key\")]`",
                ));
            }
        }
    }
    Ok(metas)
}

fn meta_list(list: &proc_macro::Group) -> Result<Vec<Meta>> {
    let mut c = Cursor::inside(list);
    let mut metas = Vec::new();
    while !c.is_empty() {
        let name = c.expect_ident("an attribute name")?;
        let value = if c.eat_punct('=').is_some() {
            match c.next() {
                Some(TokenTree::Literal(lit)) => Some(lit),
                Some(other) => {
                    return Err(Error::new(
                        other.span(),
                        format!("`{name}` takes a string literal: `{name} = \"..\"`"),
                    ));
                }
                None => {
                    return Err(Error::new(
                        c.span(),
                        format!("`{name}` takes a string literal: `{name} = \"..\"`"),
                    ));
                }
            }
        } else {
            None
        };
        if let Some(tt) = c.peek()
            && !matches!(tt, TokenTree::Punct(p) if p.as_char() == ',')
        {
            return Err(Error::new(
                tt.span(),
                format!("expected `,` or the end of the list after `{name}`"),
            ));
        }
        c.eat_punct(',');
        metas.push(Meta { name, value });
    }
    Ok(metas)
}

/// Step over `pub`, `pub(crate)`, `pub(in path)`.
fn visibility(c: &mut Cursor) {
    if c.eat_ident("pub").is_some() {
        c.eat_group(Delimiter::Parenthesis);
    }
}

/// The parameters between a type's `<` and `>`, each with its own bounds and
/// without its default.
fn params(tokens: Vec<TokenTree>) -> Result<Vec<Param>> {
    let mut out = Vec::new();
    for piece in split_commas(tokens) {
        // Attributes on a generic parameter are legal and mean nothing here.
        let mut piece = piece;
        while matches!(piece.first(), Some(TokenTree::Punct(p)) if p.as_char() == '#') {
            piece.drain(..2);
        }
        let Some(first) = piece.first().cloned() else {
            continue;
        };
        let without_default = match find_punct(&piece, '=') {
            Some(eq) => piece[..eq].to_vec(),
            None => piece,
        };
        match first {
            TokenTree::Punct(p) if p.as_char() == '\'' => {
                let (name, rest) = without_default.split_at(2);
                let bounds = match rest.first() {
                    Some(TokenTree::Punct(colon)) if colon.as_char() == ':' => rest[1..].to_vec(),
                    _ => Vec::new(),
                };
                out.push(Param::Lifetime {
                    name: [name[0].clone(), name[1].clone()],
                    bounds,
                });
            }
            TokenTree::Ident(kw) if kw.to_string() == "const" => {
                let name = match without_default.get(1) {
                    Some(TokenTree::Ident(n)) => n.clone(),
                    _ => return Err(Error::new(kw.span(), "expected a const parameter name")),
                };
                out.push(Param::Const {
                    name,
                    decl: without_default,
                });
            }
            TokenTree::Ident(name) => {
                let bounds = match without_default.get(1) {
                    Some(TokenTree::Punct(colon)) if colon.as_char() == ':' => {
                        without_default[2..].to_vec()
                    }
                    _ => Vec::new(),
                };
                out.push(Param::Type { name, bounds });
            }
            other => return Err(Error::new(other.span(), "expected a generic parameter")),
        }
    }
    Ok(out)
}

/// Fold a `where` clause into the parameters it bounds. The declaration
/// macros take bounds inline and nothing else, so a predicate on anything but
/// a parameter of the type has nowhere to go.
fn where_clause(c: &mut Cursor, generics: &mut [Param], at: Span) -> Result<()> {
    let mut tokens = Vec::new();
    while let Some(tt) = c.peek() {
        if matches!(tt, TokenTree::Group(g) if g.delimiter() == Delimiter::Brace) {
            break;
        }
        tokens.push(tt.clone());
        c.next();
    }
    if tokens.is_empty() {
        return Err(Error::new(at, "expected a predicate after `where`"));
    }
    for predicate in split_commas(tokens) {
        let is_lifetime =
            matches!(predicate.first(), Some(TokenTree::Punct(p)) if p.as_char() == '\'');
        let head = if is_lifetime { 2 } else { 1 };
        let target = match (&predicate[..], predicate.get(head)) {
            ([TokenTree::Ident(id), ..], Some(TokenTree::Punct(colon)))
                if !is_lifetime && colon.as_char() == ':' =>
            {
                generics.iter_mut().find(|p| matches!(p, Param::Type { name, .. } if name.to_string() == id.to_string()))
            }
            ([_, TokenTree::Ident(id), ..], Some(TokenTree::Punct(colon)))
                if is_lifetime && colon.as_char() == ':' =>
            {
                generics.iter_mut().find(|p| matches!(p, Param::Lifetime { name, .. } if name[1].to_string() == id.to_string()))
            }
            _ => None,
        };
        let Some(param) = target else {
            return Err(Error::new(
                predicate[0].span(),
                "this predicate cannot be moved onto a parameter. The \
                 declaration macros take bounds inline, so `where` may bound \
                 the type's own parameters, as `T: Bound` or `'a: 'b`, and \
                 nothing else",
            ));
        };
        let extra = predicate[head + 1..].to_vec();
        let bounds = match param {
            Param::Type { bounds, .. } | Param::Lifetime { bounds, .. } => bounds,
            Param::Const { .. } => unreachable!("a const parameter is never matched"),
        };
        if !bounds.is_empty() {
            bounds.push(TokenTree::Punct(proc_macro::Punct::new(
                '+',
                proc_macro::Spacing::Alone,
            )));
        }
        bounds.extend(extra);
    }
    Ok(())
}

fn fields(body: &proc_macro::Group) -> Result<Vec<Field>> {
    let mut c = Cursor::inside(body);
    let mut out = Vec::new();
    while !c.is_empty() {
        let attrs = attributes(&mut c)?;
        visibility(&mut c);
        let name = c.expect_ident("a field name")?;
        c.expect_punct(':', "`:` after the field name")?;
        c.until_comma();
        out.push(Field { attrs, name });
    }
    Ok(out)
}

fn variants(body: &proc_macro::Group) -> Result<Vec<Variant>> {
    let mut c = Cursor::inside(body);
    let mut out = Vec::new();
    while !c.is_empty() {
        let attrs = attributes(&mut c)?;
        visibility(&mut c);
        let name = c.expect_ident("a variant name")?;
        let payload = if let Some(g) = c.eat_group(Delimiter::Parenthesis) {
            let count = split_commas(g.stream().into_iter().collect()).len();
            match count {
                1 => Payload::One,
                _ => Payload::Many(g.span()),
            }
        } else if let Some(g) = c.eat_group(Delimiter::Brace) {
            Payload::Named(g.span())
        } else {
            Payload::Unit
        };
        // A discriminant is a Rust-side number and says nothing about the
        // wire, where a variant is its name.
        if c.eat_punct('=').is_some() {
            c.until_comma();
        } else {
            c.eat_punct(',');
        }
        out.push(Variant {
            attrs,
            name,
            payload,
        });
    }
    Ok(out)
}
