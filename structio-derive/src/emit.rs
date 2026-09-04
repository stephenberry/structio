//! From the shape of the type to the declaration macro that describes it.
//!
//! Every token that came from the user goes out under its own span, so an
//! error the macro raises about a field points at that field. Tokens the
//! derive adds, the macro's name and the bounds it appends, are spanned at
//! the type's name.

use proc_macro::{Delimiter, Group, Ident, Literal, Punct, Spacing, Span, TokenStream, TokenTree};

use crate::attr::{self, Format};
use crate::parse::{Input, Param, Payload, Shape};
use crate::{Error, Result};

pub(crate) fn expand(input: &Input) -> Result<TokenStream> {
    let is_enum = matches!(input.shape, Shape::Enum(_));
    let container = attr::container(&input.attrs, is_enum)?;
    let at = input.name.span();

    let mut out = Out::new(at);
    let root = root_path(container.krate.as_ref(), at)?;

    let (macro_name, header_case, header_tag, body) = match &input.shape {
        Shape::Struct(fields) if container.array.is_some() => (
            per_format(container.format, "array"),
            None,
            None,
            array_body(fields, container.element.as_ref(), at)?,
        ),
        Shape::Struct(fields) => (
            per_format(container.format, "object"),
            container.rename_all.clone(),
            None,
            object_body(fields, at)?,
        ),
        Shape::Enum(variants) => {
            if variants.is_empty() {
                return Err(Error::new(
                    at,
                    "an enum with no variants has no value to write",
                ));
            }
            let all_unit = variants.iter().all(|v| matches!(v.payload, Payload::Unit));
            // `unit_enum!` has no one-format form. Its JSON half is the
            // tagged macro's, and its BEVE half differs only in packing a run
            // of the enum as a string array, which the tagged form reads
            // back all the same, so a narrowed unit enum is a tagged one.
            let name = if all_unit && container.tag.is_none() && container.format == Format::Both {
                "unit_enum"
            } else {
                per_format(container.format, "tagged_enum")
            };
            (
                name,
                container.rename_all.clone(),
                container.tag.clone(),
                enum_body(variants, at)?,
            )
        }
    };

    out.extend(root.clone());
    out.path_sep();
    out.ident(macro_name);
    out.punct('!', Spacing::Alone);

    let mut call = Out::new(at);
    if !input.generics.is_empty() {
        call.group(
            Delimiter::Bracket,
            impl_generics(&input.generics, &root, container.format, at),
        );
    }
    call.extend(type_tokens(input));
    if header_case.is_some() || header_tag.is_some() {
        call.ident("as");
        if let Some(case) = header_case {
            call.lit(case);
        }
        if let Some(tag) = header_tag {
            call.ident("tag");
            call.lit(tag);
        }
    }
    call.extend(body);

    out.group(Delimiter::Parenthesis, call.tokens);
    out.punct(';', Spacing::Alone);
    Ok(out.tokens.into_iter().collect())
}

/// `::structio`, or the path `crate = ".."` gave.
fn root_path(krate: Option<&Literal>, at: Span) -> Result<Vec<TokenTree>> {
    match krate {
        Some(lit) => {
            let text = attr::string_content(lit)?;
            let stream: TokenStream = text.parse().map_err(|_| {
                Error::new(lit.span(), "`crate` takes a path, such as `my_structio`")
            })?;
            let tokens: Vec<TokenTree> = stream.into_iter().collect();
            if tokens.is_empty() {
                return Err(Error::new(
                    lit.span(),
                    "`crate` takes a path, such as `my_structio`",
                ));
            }
            Ok(tokens)
        }
        None => {
            let mut out = Out::new(at);
            out.path_sep();
            out.ident("structio");
            Ok(out.tokens)
        }
    }
}

fn per_format(format: Format, base: &'static str) -> &'static str {
    match (format, base) {
        (Format::Both, b) => b,
        (Format::Json, "object") => "json_object",
        (Format::Json, "array") => "json_array",
        (Format::Json, _) => "json_tagged_enum",
        (Format::Beve, "object") => "beve_object",
        (Format::Beve, "array") => "beve_array",
        (Format::Beve, _) => "beve_tagged_enum",
    }
}

/// The bracketed impl generics: each parameter as declared, with the format's
/// read-and-write bound appended to every type parameter, since the impls
/// read and write through it.
fn impl_generics(params: &[Param], root: &[TokenTree], format: Format, at: Span) -> Vec<TokenTree> {
    let mut out = Out::new(at);
    for (i, param) in params.iter().enumerate() {
        if i > 0 {
            out.punct(',', Spacing::Alone);
        }
        match param {
            Param::Lifetime { name, bounds } => {
                out.extend(name.iter().cloned());
                if !bounds.is_empty() {
                    out.punct(':', Spacing::Alone);
                    out.extend(bounds.iter().cloned());
                }
            }
            Param::Const { decl, .. } => out.extend(decl.iter().cloned()),
            Param::Type { name, bounds } => {
                out.tokens.push(TokenTree::Ident(name.clone()));
                out.punct(':', Spacing::Alone);
                if !bounds.is_empty() {
                    out.extend(bounds.iter().cloned());
                    out.punct('+', Spacing::Alone);
                }
                out.extend(root.iter().cloned());
                out.path_sep();
                match format {
                    Format::Both => {}
                    Format::Json => {
                        out.ident("json");
                        out.path_sep();
                    }
                    Format::Beve => {
                        out.ident("beve");
                        out.path_sep();
                    }
                }
                out.ident("ReadWrite");
                out.punct('+', Spacing::Alone);
                out.path_sep();
                out.ident("core");
                out.path_sep();
                out.ident("default");
                out.path_sep();
                out.ident("Default");
            }
        }
    }
    out.tokens
}

/// `Name<'a, T, N>`, or `Name` alone.
fn type_tokens(input: &Input) -> Vec<TokenTree> {
    let mut out = Out::new(input.name.span());
    out.tokens.push(TokenTree::Ident(input.name.clone()));
    if input.generics.is_empty() {
        return out.tokens;
    }
    out.punct('<', Spacing::Alone);
    for (i, param) in input.generics.iter().enumerate() {
        if i > 0 {
            out.punct(',', Spacing::Alone);
        }
        match param {
            Param::Lifetime { name, .. } => out.extend(name.iter().cloned()),
            Param::Type { name, .. } | Param::Const { name, .. } => {
                out.tokens.push(TokenTree::Ident(name.clone()));
            }
        }
    }
    out.punct('>', Spacing::Alone);
    out.tokens
}

/// `{ #[required] "key" => field as With, .., }`
fn object_body(fields: &[crate::parse::Field], at: Span) -> Result<Vec<TokenTree>> {
    let mut body = Out::new(at);
    let mut skipped = false;
    for field in fields {
        let opts = attr::field(&field.attrs, false)?;
        if opts.skip.is_some() {
            skipped = true;
            continue;
        }
        if let Some(required) = opts.required {
            body.punct('#', Spacing::Alone);
            let mut marker = Out::new(required);
            marker.ident("required");
            body.group(Delimiter::Bracket, marker.tokens);
        }
        if let Some(key) = opts.rename {
            body.lit(key);
            body.punct('=', Spacing::Joint);
            body.punct('>', Spacing::Alone);
        }
        body.tokens.push(TokenTree::Ident(field.name.clone()));
        if let Some(with) = opts.with {
            body.ident("as");
            body.extend(adapter(&with)?);
        }
        body.punct(',', Spacing::Alone);
    }
    if skipped {
        body.rest();
    }
    let mut out = Out::new(at);
    out.group(Delimiter::Brace, body.tokens);
    Ok(out.tokens)
}

/// `[ Elem ; a, b, c, .. ]`
fn array_body(
    fields: &[crate::parse::Field],
    element: Option<&Literal>,
    at: Span,
) -> Result<Vec<TokenTree>> {
    let mut body = Out::new(at);
    if let Some(element) = element {
        body.extend(adapter(element)?);
        body.punct(';', Spacing::Alone);
    }
    let mut skipped = false;
    for field in fields {
        let opts = attr::field(&field.attrs, true)?;
        if opts.skip.is_some() {
            skipped = true;
            continue;
        }
        body.tokens.push(TokenTree::Ident(field.name.clone()));
        body.punct(',', Spacing::Alone);
    }
    if skipped {
        body.rest();
    }
    let mut out = Out::new(at);
    out.group(Delimiter::Bracket, body.tokens);
    Ok(out.tokens)
}

/// `{ "name" => Variant(_), Unit, }`
fn enum_body(variants: &[crate::parse::Variant], at: Span) -> Result<Vec<TokenTree>> {
    let mut body = Out::new(at);
    for variant in variants {
        let opts = attr::variant(&variant.attrs)?;
        match variant.payload {
            Payload::Unit | Payload::One => {}
            Payload::Many(span) => {
                return Err(Error::new(
                    span,
                    "a variant carries one value: the tag wraps one payload, \
                     and the payload is a type of its own. Give these fields a \
                     struct, or a tuple",
                ));
            }
            Payload::Named(span) => {
                return Err(Error::new(
                    span,
                    "a variant with named fields is a stage 2 shape and this \
                     derive implements stage 1; give the fields a struct \
                     declared on its own, and see docs/derive.md",
                ));
            }
        }
        if let Some(name) = opts.rename {
            body.lit(name);
            body.punct('=', Spacing::Joint);
            body.punct('>', Spacing::Alone);
        }
        body.tokens.push(TokenTree::Ident(variant.name.clone()));
        if matches!(variant.payload, Payload::One) {
            let mut hole = Out::new(variant.name.span());
            hole.ident("_");
            body.group(Delimiter::Parenthesis, hole.tokens);
        }
        body.punct(',', Spacing::Alone);
    }
    let mut out = Out::new(at);
    out.group(Delimiter::Brace, body.tokens);
    Ok(out.tokens)
}

/// The tokens of a type named in a string, `with = "Vec<Millis>"`, each
/// spanned at the string so a type that does not exist is reported there.
fn adapter(lit: &Literal) -> Result<Vec<TokenTree>> {
    let text = attr::string_content(lit)?;
    let stream: TokenStream = text
        .parse()
        .map_err(|_| Error::new(lit.span(), "expected a type"))?;
    let tokens: Vec<TokenTree> = stream
        .into_iter()
        .map(|tt| respan(tt, lit.span()))
        .collect();
    if tokens.is_empty() {
        return Err(Error::new(lit.span(), "expected a type"));
    }
    Ok(tokens)
}

fn respan(tt: TokenTree, span: Span) -> TokenTree {
    match tt {
        TokenTree::Group(g) => {
            let inner: TokenStream = g.stream().into_iter().map(|t| respan(t, span)).collect();
            let mut g = Group::new(g.delimiter(), inner);
            g.set_span(span);
            TokenTree::Group(g)
        }
        mut other => {
            other.set_span(span);
            other
        }
    }
}

/// A token builder that stamps one span on everything the derive adds.
struct Out {
    tokens: Vec<TokenTree>,
    span: Span,
}

impl Out {
    fn new(span: Span) -> Self {
        Out {
            tokens: Vec::new(),
            span,
        }
    }

    fn ident(&mut self, name: &str) {
        self.tokens
            .push(TokenTree::Ident(Ident::new(name, self.span)));
    }

    fn punct(&mut self, ch: char, spacing: Spacing) {
        let mut p = Punct::new(ch, spacing);
        p.set_span(self.span);
        self.tokens.push(TokenTree::Punct(p));
    }

    fn path_sep(&mut self) {
        self.punct(':', Spacing::Joint);
        self.punct(':', Spacing::Alone);
    }

    /// The `..` that tells the macro the omission of a field is deliberate.
    fn rest(&mut self) {
        self.punct('.', Spacing::Joint);
        self.punct('.', Spacing::Alone);
    }

    fn lit(&mut self, lit: Literal) {
        self.tokens.push(TokenTree::Literal(lit));
    }

    fn group(&mut self, delimiter: Delimiter, tokens: Vec<TokenTree>) {
        let mut g = Group::new(delimiter, tokens.into_iter().collect());
        g.set_span(self.span);
        self.tokens.push(TokenTree::Group(g));
    }

    fn extend(&mut self, tokens: impl IntoIterator<Item = TokenTree>) {
        self.tokens.extend(tokens);
    }
}
