//! What a sink writer's buffer size actually bounds.
//!
//! `to_writer` documents a peak memory figure, and a figure like that is only
//! worth stating if something checks it. Nothing else observes the difference:
//! an oversized block leaves for the sink in its own write either way, so the
//! call pattern is the same and only the size the buffer grew to differs.
//!
//! The streaming reader makes the mirror-image claim -- that memory follows the
//! largest single value rather than the size of the file -- and it is checked
//! here for the same reason.
//!
//! This is its own test binary because the counter below is process wide.
//! Another test allocating on another thread would be counted here too, so the
//! tests here take a lock for their whole bodies and their bounds are still
//! generous: the harness itself allocates on threads no lock here reaches.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use structio::beve;

// ---------------------------------------------------------------------------
// A global allocator that remembers how far it got
// ---------------------------------------------------------------------------

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Tracking;

/// Records the high-water mark of live bytes. Called from inside the
/// allocator, so it must not allocate: two atomics and nothing else.
fn note(delta: isize) {
    let live = if delta >= 0 {
        LIVE.fetch_add(delta as usize, Ordering::Relaxed) + delta as usize
    } else {
        LIVE.fetch_sub(delta.unsigned_abs(), Ordering::Relaxed) - delta.unsigned_abs()
    };
    PEAK.fetch_max(live, Ordering::Relaxed);
}

// SAFETY: every method forwards to `System` unchanged and returns exactly what
// it returned. The accounting reads only the layouts it is passed.
unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            note(layout.size() as isize);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        note(-(layout.size() as isize));
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            note(new_size as isize - layout.size() as isize);
        }
        p
    }
}

#[global_allocator]
static ALLOC: Tracking = Tracking;

/// Held for the whole of each test below, building the document included: two
/// measurements running at once would each be counting the other's work.
static SERIAL: Mutex<()> = Mutex::new(());

/// The lock, tolerating a panic in another test rather than compounding it.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------

#[derive(Default)]
struct Blob {
    text: String,
    tail: String,
}
structio::object!(Blob { text, tail });

/// A sink that keeps nothing, so the only allocation under measurement is the
/// writer's own buffer.
struct Discard(usize);

impl std::io::Write for Discard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A sink writer's buffer is a bound, not a starting size.
///
/// A value larger than the buffer is one contiguous block, and copying it in
/// would mean growing the buffer to hold a run that is handed over unchanged a
/// moment later. So peak memory used to follow the longest string or typed
/// array in the document rather than the configured buffer.
///
/// The value is a string because that is one block on every target. A numeric
/// typed array is the same block on a little-endian host but is written
/// element by element on a big-endian one, which would make this say nothing
/// there.
///
/// Ignored under Miri: this is about how many bytes are asked for, which Miri
/// does not change, and a multi-megabyte value would cost minutes there.
#[test]
#[cfg_attr(miri, ignore)]
fn a_sink_writer_does_not_grow_to_hold_the_largest_value() {
    let _serial = serial();
    const CAP: usize = 512;
    const BIG: usize = 4 * 1024 * 1024;

    // Built before the measurement starts, so the document's own bytes are
    // part of the baseline rather than of what writing it cost.
    let value = Blob {
        text: "x".repeat(BIG),
        tail: "y".repeat(40),
    };
    let mut sink = Discard(0);

    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    beve::to_writer_buffered(&value, &mut sink, CAP).unwrap();
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);

    assert_eq!(
        sink.0,
        structio::to_beve(&value).len(),
        "wrote the wrong bytes"
    );
    // Generous next to the 512 bytes the writer actually asks for, because the
    // test harness may allocate on another thread while this runs, and still
    // three orders of magnitude under the 4 MiB a buffered payload would take.
    assert!(
        peak < 64 * 1024,
        "writing a {BIG}-byte value through a {CAP}-byte buffer peaked at {peak} bytes"
    );
}

/// Streaming a file holds one value of it, not the file.
///
/// Nothing else observes this either: the values come back the same whether
/// they were cut out of a small window or out of the whole document resident in
/// memory. Only the bytes asked for differ, which is exactly what the allocator
/// above counts.
///
/// The array is typed, which is the harder case and the one BEVE most needs:
/// its elements carry no headers, so a splitter that could not supply the one
/// the array implied would have to buffer the block whole.
///
/// Ignored under Miri for the same reason as above: this is about how many
/// bytes are asked for, and a multi-megabyte document would cost minutes there.
#[test]
#[cfg_attr(miri, ignore)]
fn streaming_an_array_does_not_hold_the_file() {
    let _serial = serial();
    const ELEMENTS: usize = 512 * 1024;

    let file = structio::to_beve(&(0..ELEMENTS).map(|i| i as f64).collect::<Vec<f64>>());
    assert!(file.len() > 4 * 1024 * 1024);
    let mut sum = 0.0;

    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let mut docs = beve::Documents::array(&file[..]);
    let mut value = 0.0f64;
    while let Some(result) = docs.next_value_into(&mut value) {
        result.unwrap();
        sum += value;
    }
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);

    assert_eq!(sum, (0..ELEMENTS).map(|i| i as f64).sum::<f64>());
    // The window is 64 KiB before a byte is read, so the bound is that plus
    // room for the harness, and still far under the 4 MiB the file occupies.
    assert!(
        peak < 512 * 1024,
        "streaming a {}-byte file peaked at {peak} bytes",
        file.len()
    );
}

/// Measuring a value allocates nothing at all.
///
/// `beve::size` claims to cost no allocation and no output, and that is the
/// whole reason to reach for it: framing from a buffer already avoids a second
/// walk, so a measurement that quietly staged the document somewhere would be
/// worse than the thing it replaces on every axis. The writer it drives has an
/// empty `Vec` and never grows it, which is a claim the counter above can
/// settle exactly rather than generously.
///
/// The value is deliberately one that costs megabytes to write, so a
/// measurement that fell back to writing could not hide inside the noise the
/// harness makes.
#[test]
#[cfg_attr(miri, ignore)]
fn measuring_a_value_allocates_nothing() {
    let _serial = serial();
    const BIG: usize = 4 * 1024 * 1024;

    let value = Blob {
        text: "x".repeat(BIG),
        tail: "y".repeat(40),
    };
    // A megabytes-long numeric array as well, since a string holds no payload
    // the aligned form would pad and so exercises none of it.
    let samples = vec![1.5f64; BIG / 8];

    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let size = beve::size(&value);
    let aligned = beve::size_aligned(&samples);
    let framed = beve::size_aligned_after(&samples, 12);
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);

    assert_eq!(size, structio::to_beve(&value).len());
    assert_eq!(aligned, structio::to_beve_aligned(&samples).len());

    let mut behind_a_header = vec![0u8; 12];
    beve::append_aligned(&samples, &mut behind_a_header);
    assert_eq!(framed, behind_a_header.len() - 12);
    // Twelve rather than a round header length. A 48-byte header is a multiple
    // of 16, the widest element BEVE has, so it moves no padding at all and
    // would leave this agreeing with the measurement at zero whether the offset
    // reached the writer or not.
    assert_ne!(framed, aligned);

    // Not a bound with room in it: three measurements of four-megabyte
    // documents ask the allocator for nothing whatsoever.
    assert_eq!(peak, 0, "measuring asked for {peak} bytes");
}

/// Assembling a listing by appending asks the allocator for nothing.
///
/// This is the whole reason `json::append` sits next to `write_into`. A value
/// that has to land behind something -- a protocol header, or the entries
/// already in a listing -- could previously only be written into a buffer of
/// its own and copied out of it, which on a wide listing is an allocation per
/// entry. Each entry here is kilobytes long, so that a buffer of its own
/// would stand out well above the bound below even though it is freed before
/// the next entry begins; the bound is loose only by the few dozen bytes the
/// harness allocates on threads the lock does not reach.
#[test]
#[cfg_attr(miri, ignore)]
fn appending_a_listing_allocates_nothing() {
    let _serial = serial();
    const ENTRIES: usize = 64;
    const ENTRY: usize = 4096;

    let value = Blob {
        text: "x".repeat(ENTRY),
        tail: "y".repeat(ENTRY),
    };
    let mut listing = Vec::with_capacity(48 + 1 + ENTRIES * (2 * ENTRY + 64));
    listing.extend_from_slice(&[0u8; 48]); // a header, already written
    listing.push(b'[');
    let ptr = listing.as_ptr();

    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    for _ in 0..ENTRIES {
        structio::append(&value, &mut listing);
        listing.push(b',');
    }
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);

    *listing.last_mut().unwrap() = b']';
    assert!(peak < 1024, "appending asked for {peak} bytes");
    // The one buffer throughout, and the header still in front of it.
    assert!(std::ptr::eq(listing.as_ptr(), ptr));
    assert_eq!(&listing[..48], &[0u8; 48]);
    assert_eq!(
        structio::from_slice::<Vec<Blob>>(&listing[48..])
            .unwrap()
            .len(),
        ENTRIES
    );
}

/// Reading one enormous array from a reader does not hold its encoded form.
///
/// This is the whole reason `read_array_into` exists next to `from_reader`.
/// Both produce the same `Vec`, and nothing else tells them apart: the
/// difference is that draining a reader into a buffer and parsing that leaves
/// the document and the vector resident at the same moment, and this puts the
/// payload into the vector's own memory as it arrives. So the figures are
/// measured side by side and compared, rather than either being asserted
/// against a constant that would drift.
///
/// Ignored under Miri as the tests above are: this is about how many bytes are
/// asked for, and a multi-megabyte document would cost minutes there.
#[test]
#[cfg_attr(miri, ignore)]
fn reading_an_array_from_a_reader_does_not_hold_the_encoding() {
    let _serial = serial();
    const ELEMENTS: usize = 512 * 1024;
    const PAYLOAD: usize = ELEMENTS * size_of::<f64>();

    let file = structio::to_beve(&(0..ELEMENTS).map(|i| i as f64).collect::<Vec<f64>>());
    assert!(file.len() > PAYLOAD);

    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let slurped: Vec<f64> = beve::from_reader(&file[..]).unwrap();
    let slurped_peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);
    drop(slurped);

    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let mut streamed: Vec<f64> = Vec::new();
    beve::read_array_into(&mut streamed, &file[..]).unwrap();
    let streamed_peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);

    assert_eq!(streamed.len(), ELEMENTS);
    assert_eq!(streamed[ELEMENTS - 1], (ELEMENTS - 1) as f64);
    // The vector ends at exactly the size the array needed rather than at the
    // doubling above it, which is what the cap on the growth is for. Note that
    // the counter above reads a `realloc` as the difference between the two
    // sizes, so a growth the allocator had to move is accounted as though it
    // were in place: what this bounds is what is held, not the instant of a
    // move.
    assert_eq!(streamed.capacity(), ELEMENTS);
    assert!(
        streamed_peak < PAYLOAD + 256 * 1024,
        "streaming a {PAYLOAD}-byte array peaked at {streamed_peak} bytes"
    );
    // Draining the reader first costs the document as well as the vector, so
    // the gap is not a marginal one.
    assert!(
        slurped_peak > streamed_peak + PAYLOAD / 2,
        "slurping peaked at {slurped_peak} bytes against {streamed_peak}"
    );
}

/// A count is not a licence to allocate.
///
/// The bytes here claim four billion `f64`, which is 32 GiB, and deliver a
/// kilobyte. Reserving on the count's word would ask for all of it before
/// noticing, so what is asserted is that the allocator was never asked for
/// anything like it.
#[test]
#[cfg_attr(miri, ignore)]
fn a_lying_count_allocates_what_arrives_and_not_what_it_claims() {
    let _serial = serial();
    use structio::beve::header;

    let mut doc = vec![header::array_of(header::CAT_FLOAT, 3)];
    let mut size = [0u8; 8];
    let used = header::encode_size(4_000_000_000, &mut size);
    doc.extend_from_slice(&size[..used]);
    doc.extend_from_slice(&[0u8; 1024]);

    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let err = beve::from_reader_array::<f64, _>(&doc[..]).unwrap_err();
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);

    assert_eq!(
        err.as_parse().unwrap().code,
        structio::ErrorCode::UnexpectedEnd
    );
    assert!(
        peak < 4 * 1024 * 1024,
        "a count of four billion elements asked for {peak} bytes"
    );
}
