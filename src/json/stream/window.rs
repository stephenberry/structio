//! Running the ordinary parser over a span the splitter named.
//!
//! Both streaming readers are the same machine with a different way of getting
//! bytes in: [`Documents`](super::Documents) pulls from an [`io::Read`],
//! [`Feed`](super::Feed) is handed chunks. Everything about the bytes
//! themselves is [`stream::Window`](crate::stream::Window), which the BEVE side
//! uses too; what is left here is the JSON-specific half, which is one call
//! into [`Parser`].
//!
//! [`io::Read`]: std::io::Read

use crate::error::ErrorCode;
use crate::json::parser::Parser;
use crate::json::traits::Read;
use crate::options::Options;

use super::StreamError;
use super::split::Splitter;

pub(crate) type Window = crate::stream::Window<Splitter>;

/// Parse the value the splitter located into a fresh `T`.
///
/// The borrow of `win` is the value's, so a type that borrows from the input
/// holds the window still until it is dropped. That is what stops the next
/// call from refilling underneath it.
pub(crate) fn parse<'a, O: Options, T: Read<'a> + Default>(
    win: &'a Window,
    span: (usize, usize),
) -> Result<T, StreamError> {
    let mut value = T::default();
    parse_into::<O, T>(win, span, &mut value)?;
    Ok(value)
}

/// Parse into an existing value, reusing whatever it already holds.
pub(crate) fn parse_into<'a, O: Options, T: Read<'a>>(
    win: &'a Window,
    (start, end): (usize, usize),
    value: &mut T,
) -> Result<(), StreamError> {
    let text = core::str::from_utf8(&win.bytes()[start..end])
        .map_err(|e| win.error_at(ErrorCode::InvalidUtf8, start + e.valid_up_to()))?;

    // Every span starts on a non-whitespace byte: the scanned modes skip
    // whitespace before the scan begins, and `Lines` trims its span at both
    // ends. So the parser opens directly on the value.
    let mut p = Parser::<O>::with_options(text);
    match p.read(value).and_then(|()| p.finish()) {
        Ok(()) => Ok(()),
        Err(code) => Err(win.error_at(code, start + p.position())),
    }
}
