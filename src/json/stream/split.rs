//! Finding one JSON value's bytes in a partially arrived stream.
//!
//! Everything on the streaming read side rests on this: a scan that can stop
//! anywhere, including in the middle of a value, and pick up where it left off
//! when more bytes turn up. It never parses. It only answers "where does the
//! next item end", and hands that span to the ordinary [`Parser`], so a
//! streamed document and a slurped one go down exactly the same code path and
//! cannot disagree about what JSON means.
//!
//! [`Parser`]: crate::json::parser::Parser

use crate::error::{ErrorCode, PResult};
use crate::json::parser::{MAX_DEPTH, is_ws, scalar_byte, skip_comment};
use crate::stream::{Framer, Split};
use crate::swar::{escape_mask, find_byte, first_match, load_u64};

/// How a byte stream is divided into JSON values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Mode {
    /// Whole JSON values, one after another, separated by optional whitespace.
    ///
    /// This is the general form: it makes no assumption about how the producer
    /// laid the values out, and accepts a single document as the one-value
    /// case.
    Values,
    /// Newline-delimited JSON: one value per line, blank lines ignored.
    ///
    /// Faster than [`Mode::Values`], because finding the boundary is a search
    /// for one byte rather than a structural scan. It is sound for the same
    /// reason the format exists: a literal newline is a control character, and
    /// JSON forbids those inside strings, so no newline can be interior to a
    /// value.
    ///
    /// That is also the one limit on
    /// [`Options::ALLOW_COMMENTS`](crate::Options::ALLOW_COMMENTS) here. A
    /// line holding nothing but whitespace and comments carries no value and
    /// is skipped as a blank one is, and a comment after the value on a line
    /// goes to the parser with it, but a block comment cannot span lines: the
    /// lines it opened and closed on are reported separately, and the values
    /// after them still arrive.
    Lines,
    /// The elements of one top-level array.
    ///
    /// The enclosing `[`, the separating commas, and the final `]` are
    /// consumed by the splitter; each item is one element.
    Array,
}

/// A resumable structural scan over one JSON value.
///
/// Only two things can hide the byte that ends a value: nesting, and strings.
/// Numbers and the three literals contain no structural bytes at all, so depth
/// plus "am I inside a string" is the whole state, and both survive being
/// interrupted at an arbitrary byte. A third joins them where the policy reads
/// comments, for the same reason and with the same property.
struct Scanner {
    /// Where the next [`Scanner::advance`] resumes.
    pos: usize,
    depth: u32,
    in_string: bool,
    /// The previous byte was a backslash inside a string, so this one is
    /// literal whatever it is.
    escaped: bool,
    /// A bare top-level scalar is open, and ends at the first byte that cannot
    /// continue it. Tracked only at depth zero: inside a container the
    /// delimiters end scalars, so their extent never has to be known.
    scalar: bool,
    /// The comment this scan is inside, if any.
    comment: Comment,
}

/// How far into a comment a scan has got.
///
/// Resumable for the same reason [`Scanner::escaped`] is: a refill can arrive
/// anywhere, including between the two bytes that close a block comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Comment {
    /// Not in one.
    No,
    /// Inside `//`, which the next newline ends.
    Line,
    /// Inside `/* */`.
    Block,
    /// Inside `/* */`, and the byte just read was a `*` that may close it.
    BlockStar,
}

impl Comment {
    /// Which comment the `/` at `at` opens. Both openers are two bytes, so the
    /// body always begins at `at + 2` and the caller steps over it itself.
    ///
    /// `None` when what is there is not a comment, and also when the buffer
    /// stops on the `/` and so does not yet say which it is. The caller leaves
    /// the `/` unconsumed either way, and only it knows whether more bytes can
    /// still arrive to settle it.
    fn begin(buf: &[u8], at: usize) -> Option<Comment> {
        match buf.get(at + 1)? {
            b'/' => Some(Comment::Line),
            b'*' => Some(Comment::Block),
            _ => None,
        }
    }

    /// Step from `i` to just past the end of the comment this is inside.
    ///
    /// `None` when the buffer runs out with it still open, leaving `self`
    /// holding enough to carry on from the bytes that follow.
    fn step(&mut self, buf: &[u8], i: usize) -> Option<usize> {
        match *self {
            // Not in one, so `i` is already past nothing. Both callers guard
            // on the state before asking; this arm makes the match total.
            Comment::No => Some(i),
            Comment::Line => {
                let end = find_byte(buf, i, b'\n')?;
                *self = Comment::No;
                // The newline is left for the whitespace it is.
                Some(end)
            }
            Comment::Block | Comment::BlockStar => {
                let mut i = i;
                while i < buf.len() {
                    let c = buf[i];
                    i += 1;
                    if *self == Comment::BlockStar && c == b'/' {
                        *self = Comment::No;
                        return Some(i);
                    }
                    *self = if c == b'*' {
                        Comment::BlockStar
                    } else {
                        Comment::Block
                    };
                }
                None
            }
        }
    }
}

/// Does this line hold a value, or only whitespace and comments?
///
/// `start` and `end` come from [`trim_ws`], so with comments off this is just
/// whether the line is empty.
///
/// The line is whole by the time this is asked, [`Splitter::next_line`] having
/// already found the newline that ends it, so the parser's own comment skipper
/// is the right one rather than the resumable [`Comment`] a growing buffer
/// needs. That also settles the two ways a line can fail to finish a comment: a
/// `//` with no newline is ended by the line, and a `/*` left open is not a
/// comment this line completes. The second counts as content, so the parser
/// reports the `/` it opened at, which is the one thing [`Mode::Lines`] cannot
/// frame.
fn line_has_value(buf: &[u8], start: usize, end: usize, comments: bool) -> bool {
    if !comments {
        return start != end;
    }
    let line = &buf[..end];
    let mut i = start;
    while i < end {
        match line[i] {
            c if is_ws(c) => i += 1,
            b'/' => match skip_comment(line, i) {
                Some(next) => i = next,
                None => return true,
            },
            _ => return true,
        }
    }
    false
}

impl Scanner {
    const fn new() -> Self {
        Scanner {
            pos: 0,
            depth: 0,
            in_string: false,
            escaped: false,
            scalar: false,
            comment: Comment::No,
        }
    }

    /// Begin a fresh value at `at`.
    fn restart(&mut self, at: usize) {
        *self = Scanner {
            pos: at,
            ..Scanner::new()
        };
    }

    /// Scan from where the last call stopped to the end of `buf`.
    ///
    /// `Ok(Some(end))` means a complete value occupies everything up to `end`.
    /// `Ok(None)` means the value is still open and more bytes are needed.
    ///
    /// `comments` is the policy, passed rather than held: it outlives a value
    /// where every field here describes where in *this* one the scan is.
    fn advance(&mut self, buf: &[u8], comments: bool) -> PResult<Option<usize>> {
        let n = buf.len();
        let mut i = self.pos;

        while i < n {
            if self.comment != Comment::No {
                match self.comment.step(buf, i) {
                    Some(next) => i = next,
                    None => {
                        i = n;
                        break;
                    }
                }
                continue;
            }

            if self.in_string {
                if self.escaped {
                    self.escaped = false;
                    i += 1;
                    continue;
                }
                // Skip ordinary string bytes eight at a time. The mask also
                // lights control characters, which are illegal here but are
                // the parser's to reject, so they are simply stepped over.
                while i + 8 <= n {
                    // SAFETY: `i + 8 <= n`, so the eight bytes are in bounds.
                    let m = escape_mask(unsafe { load_u64(buf, i) });
                    if m != 0 {
                        i += first_match(m);
                        break;
                    }
                    i += 8;
                }
                if i >= n {
                    break;
                }
                match buf[i] {
                    b'\\' => {
                        self.escaped = true;
                        i += 1;
                    }
                    b'"' => {
                        self.in_string = false;
                        i += 1;
                        if self.depth == 0 {
                            self.pos = i;
                            return Ok(Some(i));
                        }
                    }
                    _ => i += 1,
                }
                continue;
            }

            let c = buf[i];
            if self.scalar {
                // Only reachable at depth zero.
                if scalar_byte(c) {
                    i += 1;
                    continue;
                }
                self.scalar = false;
                self.pos = i;
                return Ok(Some(i));
            }

            // A comment can hold anything, braces and quotes included, so it
            // has to be stepped over rather than scanned through. Under a
            // policy that reads none, this is gone and a `/` is what it was.
            if comments && c == b'/' {
                match Comment::begin(buf, i) {
                    Some(kind) => {
                        self.comment = kind;
                        i += 2;
                        continue;
                    }
                    // Either a `/` that begins nothing, which the parser will
                    // reject once this value's bytes reach it, or one the
                    // buffer stops on. Breaking leaves it unconsumed, so the
                    // refill that decides it resumes here.
                    None if i + 1 >= n => break,
                    None => {}
                }
            }

            match c {
                _ if is_ws(c) => i += 1,
                b'"' => {
                    self.in_string = true;
                    i += 1;
                }
                b'{' | b'[' => {
                    self.depth += 1;
                    if self.depth > MAX_DEPTH {
                        self.pos = i;
                        return Err(ErrorCode::ExceededMaxDepth);
                    }
                    i += 1;
                }
                b'}' | b']' => {
                    if self.depth == 0 {
                        self.pos = i;
                        return Err(ErrorCode::UnexpectedCharacter);
                    }
                    self.depth -= 1;
                    i += 1;
                    if self.depth == 0 {
                        self.pos = i;
                        return Ok(Some(i));
                    }
                }
                _ if self.depth == 0 => {
                    if !scalar_byte(c) {
                        self.pos = i;
                        return Err(ErrorCode::UnexpectedCharacter);
                    }
                    self.scalar = true;
                    i += 1;
                }
                // Inside a container: commas, colons, and scalar bytes all
                // just advance.
                _ => i += 1,
            }
        }

        self.pos = i;
        Ok(None)
    }

    /// No more bytes are coming.
    ///
    /// A bare scalar is the one value that ends at end of input rather than at
    /// a byte of its own, so it completes here. Anything else still open was
    /// truncated.
    ///
    /// `standalone` says whether a scalar may end this way at all. Inside an
    /// array it may not: the element sits at depth zero because the splitter
    /// consumed the `[` itself, but the array still owes a `]`, so input that
    /// stops mid-element is truncated however complete the element looks.
    fn at_eof(&mut self, standalone: bool) -> PResult<usize> {
        if self.scalar && standalone {
            self.scalar = false;
            return Ok(self.pos);
        }
        Err(ErrorCode::UnexpectedEnd)
    }
}

/// Where the array driver is between elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Looking for the opening `[`.
    BeforeOpen,
    /// Looking for the first element, or the `]` of an empty array.
    FirstItem,
    /// Looking for the start of a value. In an array this is only reached
    /// after a comma, so a `]` here is a trailing comma, and it is left to the
    /// scan to reject it exactly as the parser would.
    BeforeItem,
    /// Scanning an element.
    InItem,
    /// Looking for the `,` or `]` that follows an element.
    AfterItem,
    /// The stream is over; only trailing whitespace may remain.
    Done,
}

/// Divides a growing byte buffer into JSON values.
///
/// The [`Framer`] impl below is the whole interface; see it for what the
/// window may discard and how it says so.
pub(crate) struct Splitter {
    mode: Mode,
    scan: Scanner,
    state: State,
    /// First byte not yet accounted for. Everything below it is dead.
    cursor: usize,
    /// How far [`Mode::Lines`] has searched for a newline, so a partial line
    /// is not rescanned from the start on every refill.
    probe: usize,
    /// [`Options::ALLOW_COMMENTS`](crate::Options::ALLOW_COMMENTS).
    comments: bool,
    /// The comment the space *between* values is inside, if any. The one
    /// inside a value belongs to [`Scanner`].
    comment: Comment,
}

impl Splitter {
    pub(crate) fn new(mode: Mode) -> Self {
        Splitter {
            mode,
            scan: Scanner::new(),
            state: match mode {
                Mode::Array => State::BeforeOpen,
                _ => State::BeforeItem,
            },
            cursor: 0,
            probe: 0,
            comments: false,
            comment: Comment::No,
        }
    }

    /// Read comments, or do not.
    ///
    /// Set from [`Options::ALLOW_COMMENTS`](crate::Options::ALLOW_COMMENTS)
    /// rather than taken as a type parameter, because a stream names its
    /// policy through `with_options` after the splitter already exists.
    ///
    /// A comment may hold a brace, a bracket, or a quote, so a splitter that
    /// did not know about one would divide the stream in the wrong place
    /// rather than merely pass it on.
    /// Belongs before any bytes reach the stream, which is where
    /// `with_options` sits in the builder chain. Setting it discards the
    /// comment a scan was inside, there being no coherent way to carry one
    /// across a change in whether comments are read at all.
    pub(crate) fn set_comments(&mut self, on: bool) {
        self.comments = on;
        self.comment = Comment::No;
        self.scan.comment = Comment::No;
    }

    /// [`Mode::Lines`]: the boundary is a newline, so no structure is tracked.
    fn next_line(&mut self, buf: &[u8], eof: bool) -> PResult<Split> {
        loop {
            let (line_end, next) = match find_byte(buf, self.probe.max(self.cursor), b'\n') {
                Some(p) => (p, p + 1),
                None if eof => (buf.len(), buf.len()),
                None => {
                    self.probe = buf.len();
                    return Ok(Split::Need);
                }
            };

            let (start, end) = trim_ws(buf, self.cursor, line_end);
            self.cursor = next;
            self.probe = next;
            if line_has_value(buf, start, end, self.comments) {
                return Ok(Split::Item { start, end });
            }
            // A line with no value on it, blank or wholly a comment, carries
            // nothing. At end of input that is the clean way for the stream to
            // stop.
            if eof && next >= buf.len() {
                return Ok(Split::End);
            }
        }
    }

    /// [`Mode::Values`] and [`Mode::Array`]: the boundary comes from the
    /// structural scan.
    fn next_scanned(&mut self, buf: &[u8], eof: bool) -> PResult<Split> {
        loop {
            match self.state {
                State::BeforeOpen => {
                    let Some(c) = self.skip_ws(buf, eof)? else {
                        // Nothing but whitespace and no more coming is an
                        // empty stream, not a malformed array.
                        return if eof { Ok(Split::End) } else { Ok(Split::Need) };
                    };
                    if c != b'[' {
                        return Err(ErrorCode::ExpectedBracket);
                    }
                    self.cursor += 1;
                    self.state = State::FirstItem;
                }

                State::FirstItem => {
                    let Some(c) = self.skip_ws(buf, eof)? else {
                        return if eof {
                            Err(ErrorCode::UnexpectedEnd)
                        } else {
                            Ok(Split::Need)
                        };
                    };
                    if c == b']' {
                        self.cursor += 1;
                        self.state = State::Done;
                        continue;
                    }
                    self.scan.restart(self.cursor);
                    self.state = State::InItem;
                }

                State::BeforeItem => {
                    if self.skip_ws(buf, eof)?.is_none() {
                        return match (eof, self.mode) {
                            // A `Values` stream ends between values.
                            (true, Mode::Values) => Ok(Split::End),
                            // An array that ran out mid-way did not close.
                            (true, _) => Err(ErrorCode::UnexpectedEnd),
                            (false, _) => Ok(Split::Need),
                        };
                    }
                    self.scan.restart(self.cursor);
                    self.state = State::InItem;
                }

                State::InItem => {
                    let end = match self.scan.advance(buf, self.comments)? {
                        Some(end) => end,
                        None if eof => self.scan.at_eof(self.mode == Mode::Values)?,
                        None => return Ok(Split::Need),
                    };
                    let start = self.cursor;
                    self.cursor = end;
                    self.state = if self.mode == Mode::Array {
                        State::AfterItem
                    } else {
                        State::BeforeItem
                    };
                    return Ok(Split::Item { start, end });
                }

                State::AfterItem => {
                    let Some(c) = self.skip_ws(buf, eof)? else {
                        return if eof {
                            Err(ErrorCode::UnexpectedEnd)
                        } else {
                            Ok(Split::Need)
                        };
                    };
                    match c {
                        b',' => {
                            self.cursor += 1;
                            self.state = State::BeforeItem;
                        }
                        b']' => {
                            self.cursor += 1;
                            self.state = State::Done;
                        }
                        _ => return Err(ErrorCode::ExpectedComma),
                    }
                }

                State::Done => {
                    // The array closed. Nothing but whitespace may follow, and
                    // that has to be confirmed before the stream can be
                    // declared clean.
                    return match self.skip_ws(buf, eof)? {
                        Some(_) => Err(ErrorCode::TrailingContent),
                        None if eof => Ok(Split::End),
                        None => Ok(Split::Need),
                    };
                }
            }
        }
    }

    /// Advance `cursor` past whitespace, and past comments where the policy
    /// reads them, returning the byte it lands on.
    ///
    /// `None` means the buffer ran out, which is only conclusive at end of
    /// input. A comment still open when the bytes run out is the same answer
    /// for the same reason, except at end of input, where a block comment that
    /// never closed is the stream stopping inside one.
    fn skip_ws(&mut self, buf: &[u8], eof: bool) -> PResult<Option<u8>> {
        let mut i = self.cursor;
        loop {
            if self.comment != Comment::No {
                match self.comment.step(buf, i) {
                    Some(next) => i = next,
                    None => {
                        // A line comment is ended by the end of input as
                        // readily as by a newline. A block one is not.
                        if eof && self.comment != Comment::Line {
                            return Err(ErrorCode::UnexpectedEnd);
                        }
                        self.cursor = buf.len();
                        return Ok(None);
                    }
                }
                continue;
            }
            match buf.get(i) {
                Some(&c) if is_ws(c) => i += 1,
                Some(b'/') if self.comments => match Comment::begin(buf, i) {
                    Some(kind) => {
                        self.comment = kind;
                        i += 2;
                    }
                    // The buffer stops on the `/`, so which it is has not
                    // arrived.
                    None if !eof && i + 1 >= buf.len() => {
                        self.cursor = i;
                        return Ok(None);
                    }
                    // Not a comment, and left where it is: what the caller
                    // says about the byte it wanted here is the better error.
                    None => break,
                },
                _ => break,
            }
        }
        self.cursor = i;
        Ok(buf.get(i).copied())
    }
}

impl Framer for Splitter {
    /// The next item, if the bytes for one have arrived.
    fn next(&mut self, buf: &[u8], eof: bool) -> PResult<Split> {
        match self.mode {
            Mode::Lines => self.next_line(buf, eof),
            _ => self.next_scanned(buf, eof),
        }
    }

    #[inline]
    fn consumed(&self) -> usize {
        self.cursor
    }

    /// `probe` only moves in `Lines` mode and `scan.pos` only in the scanned
    /// modes, so in either mode the other sits at or behind the cursor and
    /// saturates to zero.
    fn rebase(&mut self) {
        let shift = self.cursor;
        self.cursor = 0;
        self.probe = self.probe.saturating_sub(shift);
        self.scan.pos = self.scan.pos.saturating_sub(shift);
    }

    /// The two error sources leave their position in different places: the
    /// delimiter checks stop the cursor on the offending byte, while the
    /// structural scan stops its own position there and leaves the cursor at
    /// the start of the item. Whichever is further along is the one that just
    /// moved.
    #[inline]
    fn position(&self) -> usize {
        self.cursor.max(self.scan.pos)
    }
}

/// Narrow `buf[start..end]` to its non-whitespace core.
fn trim_ws(buf: &[u8], mut start: usize, mut end: usize) -> (usize, usize) {
    while start < end && is_ws(buf[start]) {
        start += 1;
    }
    while end > start && is_ws(buf[end - 1]) {
        end -= 1;
    }
    (start, end)
}
