//! The optional `#[derive(Structio)]` for [structio](https://docs.rs/structio).
//!
//! Enable structio's `derive` feature and write `#[derive(structio::Structio)]`
//! rather than depending on this crate by name. The derive is a front end to
//! the declaration macros: it reads the type and emits the `object!`,
//! `array!`, `unit_enum!` or `tagged_enum!` invocation you would have written,
//! with the attributes translated to that macro's syntax. The impls, the key
//! map, the required-field mask and every rule about what is accepted are
//! the macros' own, so a derived type and a declared type behave identically.
//!
//! The attributes and what each expands to are documented at
//! [`docs/derive.md`](https://github.com/stephenberry/structio/blob/main/docs/derive.md).
//!
//! This crate has no dependencies. It walks `proc_macro::TokenStream` itself,
//! which is a few hundred lines for the shapes it has to recognize and keeps
//! the derive's own build under a second.

mod attr;
mod cursor;
mod emit;
mod parse;

use proc_macro::{Delimiter, Group, Ident, Literal, Punct, Spacing, Span, TokenStream, TokenTree};

/// A refusal, pointed at the token that caused it.
pub(crate) struct Error {
    span: Span,
    message: String,
}

impl Error {
    pub(crate) fn new(span: Span, message: impl Into<String>) -> Self {
        Error {
            span,
            message: message.into(),
        }
    }

    /// `::core::compile_error!("message")` with every token at the span, so
    /// the diagnostic lands on the user's attribute or field and not on the
    /// derive.
    fn into_compile_error(self) -> TokenStream {
        let span = self.span;
        let at = |mut tt: TokenTree| {
            tt.set_span(span);
            tt
        };
        let punct = |ch, spacing| at(TokenTree::Punct(Punct::new(ch, spacing)));
        let mut message = Literal::string(&self.message);
        message.set_span(span);
        let tokens = [
            punct(':', Spacing::Joint),
            punct(':', Spacing::Alone),
            at(TokenTree::Ident(Ident::new("core", span))),
            punct(':', Spacing::Joint),
            punct(':', Spacing::Alone),
            at(TokenTree::Ident(Ident::new("compile_error", span))),
            punct('!', Spacing::Alone),
            at(TokenTree::Group(Group::new(
                Delimiter::Brace,
                TokenStream::from(TokenTree::Literal(message)),
            ))),
        ];
        tokens.into_iter().collect()
    }
}

pub(crate) type Result<T> = std::result::Result<T, Error>;

/// Declare a struct's or enum's schema from its definition.
///
/// See [structio's documentation](https://docs.rs/structio) for the attributes.
/// In short: `#[structio(rename_all = "camelCase")]`, `#[structio(tag =
/// "kind")]`, `#[structio(array)]`, `#[structio(json)]` or `#[structio(beve)]`
/// on the type; `#[structio(rename = "key")]`, `#[structio(skip)]`,
/// `#[structio(required)]` and `#[structio(with = "Adapter")]` on a field;
/// `#[structio(rename = "name")]` on a variant.
#[proc_macro_derive(Structio, attributes(structio))]
pub fn derive_structio(input: TokenStream) -> TokenStream {
    match parse::parse(input).and_then(|input| emit::expand(&input)) {
        Ok(tokens) => tokens,
        Err(error) => error.into_compile_error(),
    }
}
