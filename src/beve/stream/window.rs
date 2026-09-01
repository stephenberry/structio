//! Running the ordinary reader over a span the splitter named.
//!
//! Everything about the bytes themselves is
//! [`stream::Window`](crate::stream::Window), which the JSON side uses too.
//! What is left here is the BEVE-specific half: one [`Reader`] over the span,
//! carrying the header the splitter says the span does not contain.

use crate::beve::reader::Reader;
use crate::beve::traits::Read;
use crate::options::Options;

use super::StreamError;
use super::split::Splitter;

pub(crate) type Window = crate::stream::Window<Splitter>;

/// Read the value the splitter located into a fresh `T`.
///
/// The borrow of `win` is the value's, so a type that borrows from the input
/// holds the window still until it is dropped. That is what makes a `&'de str`
/// field work here: it points into the window, and the window cannot refill
/// underneath it.
pub(crate) fn read<'a, O: Options, T: Read<'a> + Default>(
    win: &'a Window,
    span: (usize, usize),
) -> Result<T, StreamError> {
    let mut value = T::default();
    read_into::<O, T>(win, span, &mut value)?;
    Ok(value)
}

/// Read into an existing value, reusing whatever it already holds.
pub(crate) fn read_into<'a, O: Options, T: Read<'a>>(
    win: &'a Window,
    (start, end): (usize, usize),
    value: &mut T,
) -> Result<(), StreamError> {
    let bytes = &win.bytes()[start..end];
    // A typed array's elements are stored without headers, so an element's
    // span is not a value on its own until the one the array implied is put
    // back in front of it.
    let mut r = match win.framer().implied() {
        Some(h) => Reader::<O>::with_implied(bytes, h),
        None => Reader::<O>::with_options(bytes),
    };
    match value.read(&mut r).and_then(|()| r.finish()) {
        Ok(()) => Ok(()),
        Err(code) => Err(win.error_at(code, start + r.position())),
    }
}
