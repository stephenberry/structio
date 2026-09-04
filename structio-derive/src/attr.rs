//! The `#[structio(..)]` attribute grammar.
//!
//! Each attribute is checked where it is written: an unknown name, a value
//! where none is taken, a rule that is not a case rule, or an attribute the
//! derive knows about but does not implement yet all fail here, at the
//! attribute, before anything is expanded.

use proc_macro::{Literal, Span};

use crate::parse::Meta;
use crate::{Error, Result};

/// Which format's impls to generate.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Both,
    Json,
    Beve,
}

pub(crate) struct Container {
    pub(crate) rename_all: Option<Literal>,
    pub(crate) tag: Option<Literal>,
    pub(crate) array: Option<Span>,
    pub(crate) element: Option<Literal>,
    pub(crate) format: Format,
    pub(crate) krate: Option<Literal>,
}

pub(crate) struct FieldOpts {
    pub(crate) rename: Option<Literal>,
    pub(crate) skip: Option<Span>,
    pub(crate) required: Option<Span>,
    pub(crate) with: Option<Literal>,
}

pub(crate) struct VariantOpts {
    pub(crate) rename: Option<Literal>,
}

/// The case rules `structio::case` accepts, spelled as the macros spell them.
/// Checked here rather than left to `__case_check!` so the error lands on the
/// attribute; the list is the one in `src/case.rs`.
const CASES: &[&str] = &[
    "lowercase",
    "UPPERCASE",
    "PascalCase",
    "camelCase",
    "snake_case",
    "SCREAMING_SNAKE_CASE",
    "kebab-case",
    "SCREAMING-KEBAB-CASE",
];

/// Attributes the derive will take in a later stage. Naming the stage tells
/// the reader what to look for, where "unknown attribute" would send them
/// hunting for a typo.
fn stage_of(name: &str) -> Option<u8> {
    match name {
        "content" | "alias" => Some(2),
        "default" | "transparent" | "write_only" | "skip_if" | "skip_read" | "skip_write" => {
            Some(3)
        }
        _ => None,
    }
}

fn unknown(meta: &Meta, place: &str, accepted: &str) -> Error {
    let name = meta.name.to_string();
    let message = match stage_of(&name) {
        Some(stage) => format!(
            "`{name}` is a stage {stage} attribute and this derive implements \
             stage 1; see docs/derive.md for what each stage adds"
        ),
        None => {
            format!("unknown attribute `{name}` on {place}; the attributes here are {accepted}")
        }
    };
    Error::new(meta.name.span(), message)
}

/// A flag takes no value.
fn flag(meta: &Meta) -> Result<Span> {
    match &meta.value {
        None => Ok(meta.name.span()),
        Some(value) => Err(Error::new(
            value.span(),
            format!("`{}` takes no value", meta.name),
        )),
    }
}

/// A setting takes a plain string literal.
fn string(meta: &Meta) -> Result<Literal> {
    match &meta.value {
        Some(value) => {
            string_content(value)?;
            Ok(value.clone())
        }
        None => Err(Error::new(
            meta.name.span(),
            format!(
                "`{name}` takes a string: `{name} = \"..\"`",
                name = meta.name
            ),
        )),
    }
}

/// The text inside a plain `"..."` literal. Escapes and raw strings are
/// refused rather than interpreted: nothing an attribute here names needs
/// them, and a key that does can be spelled at the macro.
pub(crate) fn string_content(lit: &Literal) -> Result<String> {
    let text = lit.to_string();
    let inner = text
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .ok_or_else(|| Error::new(lit.span(), "expected a plain string literal"))?;
    if inner.contains('\\') {
        return Err(Error::new(
            lit.span(),
            "an escape in an attribute string is not supported; write the \
             characters themselves",
        ));
    }
    Ok(inner.to_string())
}

fn once<T>(slot: &mut Option<T>, meta: &Meta, value: T) -> Result<()> {
    if slot.is_some() {
        return Err(Error::new(
            meta.name.span(),
            format!("`{}` is given twice", meta.name),
        ));
    }
    *slot = Some(value);
    Ok(())
}

pub(crate) fn container(metas: &[Meta], is_enum: bool) -> Result<Container> {
    let mut out = Container {
        rename_all: None,
        tag: None,
        array: None,
        element: None,
        format: Format::Both,
        krate: None,
    };
    let mut format_at: Option<Span> = None;
    let (place, accepted) = if is_enum {
        ("an enum", "`rename_all`, `tag`, `json`, `beve` and `crate`")
    } else {
        (
            "a struct",
            "`rename_all`, `array`, `element`, `json`, `beve` and `crate`",
        )
    };
    for meta in metas {
        match meta.name.to_string().as_str() {
            "rename_all" => {
                let lit = string(meta)?;
                let rule = string_content(&lit)?;
                if !CASES.contains(&rule.as_str()) {
                    return Err(Error::new(
                        lit.span(),
                        format!(
                            "`{rule}` is not a case rule; the rules are {}",
                            CASES
                                .iter()
                                .map(|c| format!("`{c}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    ));
                }
                once(&mut out.rename_all, meta, lit)?;
            }
            "tag" if is_enum => once(&mut out.tag, meta, string(meta)?)?,
            "tag" => {
                return Err(Error::new(
                    meta.name.span(),
                    "`tag` names the member an enum's variant name is written \
                     under; a struct has no variant to tag",
                ));
            }
            "array" if !is_enum => once(&mut out.array, meta, flag(meta)?)?,
            "element" if !is_enum => once(&mut out.element, meta, string(meta)?)?,
            "array" | "element" => {
                return Err(Error::new(
                    meta.name.span(),
                    "`array` declares a struct's fields by position; an enum \
                     is written as its variant's name",
                ));
            }
            "json" | "beve" => {
                let at = flag(meta)?;
                if format_at.is_some() {
                    return Err(Error::new(
                        at,
                        "`json` and `beve` each narrow the derive to one \
                         format; leave both out to generate for both",
                    ));
                }
                format_at = Some(at);
                out.format = if meta.name.to_string() == "json" {
                    Format::Json
                } else {
                    Format::Beve
                };
            }
            "crate" => once(&mut out.krate, meta, string(meta)?)?,
            _ => return Err(unknown(meta, place, accepted)),
        }
    }
    if let Some(element) = &out.element
        && out.array.is_none()
    {
        return Err(Error::new(
            element.span(),
            "`element` names the type every element of a positional struct \
             has, so it goes with `array`",
        ));
    }
    if let (Some(rule), Some(_)) = (&out.rename_all, out.array) {
        return Err(Error::new(
            rule.span(),
            "a positional struct writes no keys, so a case rule has nothing \
             to convert",
        ));
    }
    Ok(out)
}

pub(crate) fn field(metas: &[Meta], positional: bool) -> Result<FieldOpts> {
    let mut out = FieldOpts {
        rename: None,
        skip: None,
        required: None,
        with: None,
    };
    for meta in metas {
        let name = meta.name.to_string();
        if positional && name != "skip" && stage_of(&name).is_none() {
            return Err(Error::new(
                meta.name.span(),
                format!(
                    "`{name}` has no meaning on a field of a positional struct: \
                     an element is found by its position, has no key, and is \
                     required by the array's length. Only `skip` applies"
                ),
            ));
        }
        match name.as_str() {
            "rename" => once(&mut out.rename, meta, string(meta)?)?,
            "skip" => once(&mut out.skip, meta, flag(meta)?)?,
            "required" => once(&mut out.required, meta, flag(meta)?)?,
            "with" => once(&mut out.with, meta, string(meta)?)?,
            _ => {
                return Err(unknown(
                    meta,
                    "a field",
                    "`rename`, `skip`, `required` and `with`",
                ));
            }
        }
    }
    if let Some(skip) = out.skip {
        let other = [
            out.rename.as_ref().map(|_| "rename"),
            out.required.as_ref().map(|_| "required"),
            out.with.as_ref().map(|_| "with"),
        ]
        .into_iter()
        .flatten()
        .next();
        if let Some(other) = other {
            return Err(Error::new(
                skip,
                format!("a skipped field is not on the wire, so `{other}` has nothing to apply to"),
            ));
        }
    }
    Ok(out)
}

pub(crate) fn variant(metas: &[Meta]) -> Result<VariantOpts> {
    let mut out = VariantOpts { rename: None };
    for meta in metas {
        match meta.name.to_string().as_str() {
            "rename" => once(&mut out.rename, meta, string(meta)?)?,
            "skip" => {
                return Err(Error::new(
                    meta.name.span(),
                    "a variant cannot be skipped: a value holding it would \
                     have nothing to write, and the declaration has to name \
                     every variant the enum has",
                ));
            }
            _ => return Err(unknown(meta, "a variant", "`rename`")),
        }
    }
    Ok(out)
}
