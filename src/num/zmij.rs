//! Shortest round-trip float digits, without a digit-removal loop.
//!
//! A port of Victor Zverovich's [zmij], by way of the copy vendored into
//! [Glaze]. The algorithm, the seed tables, and the SWAR digit split are his
//! work, both under the MIT license.
//!
//! Ryu, which this replaces, sheds redundant digits in a data-dependent
//! division loop, so it is slowest on the short exact decimals real documents
//! are full of. `docs/design.md` carries that comparison.
//!
//! Zmij never loops. One 128x64 multiply against a power of ten yields a
//! fixed-width significand, sixteen digits for `f64` and eight for `f32`, plus
//! one more digit held back in [`Decimal::last_digit`]. The rounding test says
//! whether that held-back digit is needed, which is the same question Ryu
//! answers by division. The significand then splits into one byte per digit
//! with three multiply-add steps, and the digit count falls out of a
//! leading-zeros count over those bytes.
//!
//! So the work is the same for every input and is about one multiply chain
//! deep. See [`super::dtoa`] for the formatting policy applied on top.
//!
//! [zmij]: https://github.com/vitaut/zmij
//! [Glaze]: https://github.com/stephenberry/glaze

/// Scratch width for the digit bytes.
///
/// Seventeen digits and a leading pad is all that is ever used; the rest is
/// headroom so [`super::dtoa`] can block copy a fixed width from any offset
/// without a bounds-dependent length. The high-water mark is a pad byte plus
/// the sixteen integral digits `MAX_FIXED_F64` allows, plus one `BLOCK`:
/// `1 + 16 + 24 = 41`. Shrinking either constant needs that redone.
pub(crate) const DIGIT_SCRATCH: usize = 48;

/// Shortest decimal for a float, as ASCII digits plus a position.
///
/// The digits are `buf[start..start + len]`, and the value they spell is
/// `d1.d2d3... * 10^e10`. There are no trailing zeros: `1e300` arrives as one
/// digit with `e10 == 300`.
pub(crate) struct Digits {
    pub buf: [u8; DIGIT_SCRATCH],
    pub start: usize,
    pub len: usize,
    /// Scientific exponent: the power of ten on the leading digit.
    pub e10: i32,
}

// ---------------------------------------------------------------------------
// Powers of ten
// ---------------------------------------------------------------------------

/// Number of powers of ten the table spans.
const NUM_POW10S: usize = 618;
/// Decimal exponent of the first table entry.
const DEC_EXP_MIN: i32 = -293;

/// Seed values for the power-of-ten table.
///
/// Each entry is `10^k` for `k` in a 28-wide window, normalized so the leading
/// bit is set. Multiplying one of these by a [`POW10_MAJOR`] entry reaches any
/// power in range, which is how 618 128-bit values compress into 51.
const POW10_MINOR: [u64; 28] = [
    0x8000000000000000,
    0xa000000000000000,
    0xc800000000000000,
    0xfa00000000000000,
    0x9c40000000000000,
    0xc350000000000000,
    0xf424000000000000,
    0x9896800000000000,
    0xbebc200000000000,
    0xee6b280000000000,
    0x9502f90000000000,
    0xba43b74000000000,
    0xe8d4a51000000000,
    0x9184e72a00000000,
    0xb5e620f480000000,
    0xe35fa931a0000000,
    0x8e1bc9bf04000000,
    0xb1a2bc2ec5000000,
    0xde0b6b3a76400000,
    0x8ac7230489e80000,
    0xad78ebc5ac620000,
    0xd8d726b7177a8000,
    0x878678326eac9000,
    0xa968163f0a57b400,
    0xd3c21bcecceda100,
    0x84595161401484a0,
    0xa56fa5b99019a5c8,
    0xcecb8f27f4200f3a,
];

/// Coarse seeds, one per 28-wide window of [`POW10_MINOR`].
const POW10_MAJOR: [[u64; 2]; 23] = [
    [0xaf8e5410288e1b6f, 0x07ecf0ae5ee44dda],
    [0xb1442798f49ffb4a, 0x99cd11cfdf41779d],
    [0xb2fe3f0b8599ef07, 0x861fa7e6dcb4aa15],
    [0xb4bca50b065abe63, 0x0fed077a756b53aa],
    [0xb67f6455292cbf08, 0x1a3bc84c17b1d543],
    [0xb84687c269ef3bfb, 0x3d5d514f40eea742],
    [0xba121a4650e4ddeb, 0x92f34d62616ce413],
    [0xbbe226efb628afea, 0x890489f70a55368c],
    [0xbdb6b8e905cb600f, 0x5400e987bbc1c921],
    [0xbf8fdb78849a5f96, 0xde98520472bdd034],
    [0xc16d9a0095928a27, 0x75b7053c0f178294],
    [0xc350000000000000, 0x0000000000000000],
    [0xc5371912364ce305, 0x6c28000000000000],
    [0xc722f0ef9d80aad6, 0x424d3ad2b7b97ef6],
    [0xc913936dd571c84c, 0x03bc3a19cd1e38ea],
    [0xcb090c8001ab551c, 0x5cadf5bfd3072cc6],
    [0xcd036837130890a1, 0x36dba887c37a8c10],
    [0xcf02b2c21207ef2e, 0x94f967e45e03f4bc],
    [0xd106f86e69d785c7, 0xe13336d701beba52],
    [0xd31045a8341ca07c, 0x1ede48111209a051],
    [0xd51ea6fa85785631, 0x552a74227f3ea566],
    [0xd732290fbacaf133, 0xa97c177947ad4096],
    [0xd94ad8b1c7380874, 0x18375281ae7822bc],
];

/// One bit per power of ten, set where the reconstruction above lands one
/// ulp high. Cheaper than storing a correction per entry.
const POW10_FIXUPS: [u32; 20] = [
    0x0a4e363f, 0x00001840, 0x00006400, 0x24200040, 0x00000000, 0x0c000000, 0x82c81380, 0x5e4ce01f,
    0xd730f60f, 0x0000001b, 0x00000000, 0xcdf7fffc, 0x6e8201d8, 0x40cd3fd1, 0xdb642501, 0x00000d0d,
    0x14042400, 0x53713840, 0x11781db4, 0x00000000,
];

/// High 64 bits of `x * y`.
#[inline(always)]
const fn umul_hi(x: u64, y: u64) -> u64 {
    ((x as u128 * y as u128) >> 64) as u64
}

/// Rebuild the `i`th 128-bit power of ten from the two seed tables.
const fn compute_pow10(i: usize) -> [u64; 2] {
    let m = POW10_MINOR[(i + 10) % POW10_MINOR.len()];
    let h = POW10_MAJOR[(i + 10) / POW10_MINOR.len()];
    let (h_hi, h_lo) = (h[0], h[1]);

    let c1_carry = umul_hi(h_lo, m);
    let c0 = h_lo.wrapping_mul(m);
    let c1 = c1_carry.wrapping_add(h_hi.wrapping_mul(m));
    let c2 = ((c1 < c1_carry) as u64).wrapping_add(umul_hi(h_hi, m));

    // Renormalize so the leading bit is set.
    let (hi, lo) = if (c2 >> 63) != 0 {
        (c2, c1)
    } else {
        (c2 << 1 | c1 >> 63, c1 << 1 | c0 >> 63)
    };
    [
        hi,
        lo.wrapping_sub(((POW10_FIXUPS[i >> 5] >> (i & 31)) & 1) as u64),
    ]
}

/// Every power of ten a finite `f64` can need, `10^-293` through `10^324`.
///
/// Rebuilt from the seeds above at compile time, so about 10 KB of table costs
/// nothing in the source. `static` rather than `const` so there is exactly one
/// instance no matter how many places read it.
static POW10: [[u64; 2]; NUM_POW10S] = {
    let mut t = [[0u64; 2]; NUM_POW10S];
    let mut i = 0;
    while i < NUM_POW10S {
        t[i] = compute_pow10(i);
        i += 1;
    }
    t
};

/// The 128-bit significand of `10^dec_exp`.
#[inline(always)]
fn pow10(dec_exp: i32) -> (u64, u64) {
    let e = POW10[(dec_exp - DEC_EXP_MIN) as usize];
    (e[0], e[1])
}

// ---------------------------------------------------------------------------
// Exponent arithmetic
// ---------------------------------------------------------------------------

/// `floor(bin_exp * log10(2))`, or `floor(bin_exp * log10(2) - log10(4/3))`
/// when the value sits at a power-of-two boundary and its rounding interval is
/// therefore lopsided.
#[inline(always)]
const fn compute_dec_exp(bin_exp: i32, regular: bool) -> i32 {
    const LOG10_2_SIG: i64 = 315_653;
    const LOG10_3_OVER_4_SIG: i64 = 131_072;
    let n = bin_exp as i64 * LOG10_2_SIG - (!regular as i64) * LOG10_3_OVER_4_SIG;
    (n >> 20) as i32
}

/// How far to shift the significand so the product lands with the wanted
/// number of integral digits.
#[inline(always)]
const fn compute_exp_shift(bin_exp: i32, dec_exp: i32) -> i32 {
    const LOG2_POW10_SIG: i32 = 217_707;
    let pow10_bin_exp = (-dec_exp * LOG2_POW10_SIG) >> 16;
    bin_exp + pow10_bin_exp + 1
}

/// Guard bits kept below the integral digits, so the fractional part has room
/// for the rounding test. Used by the `f64` fast path and by the irregular
/// path of both widths; only [`F32_EXTRA_SHIFT`] is specific to one width.
const EXTRA_SHIFT: i32 = 6;

/// [`compute_exp_shift`] for every `f64` exponent, since it is on the critical
/// path and the input is only eleven bits wide.
static EXP_SHIFTS: [u8; 2048] = {
    let mut t = [0u8; 2048];
    let mut raw_exp = 0i32;
    while raw_exp < 2048 {
        // The subnormal slot is unused; the subnormal path enters with a raw
        // exponent of 1, which is the same binary exponent.
        let bin_exp = raw_exp - EXP_OFFSET_F64 + (raw_exp == 0) as i32;
        let dec_exp = compute_dec_exp(bin_exp, true);
        t[raw_exp as usize] = (compute_exp_shift(bin_exp, dec_exp + 1) + EXTRA_SHIFT) as u8;
        raw_exp += 1;
    }
    t
};

// ---------------------------------------------------------------------------
// Wide multiplies
// ---------------------------------------------------------------------------

/// High 128 bits of a 128x64 product.
#[inline(always)]
fn umul192_hi128(x_hi: u64, x_lo: u64, y: u64) -> (u64, u64) {
    let p = x_hi as u128 * y as u128;
    let p_lo = p as u64;
    let lo = p_lo.wrapping_add(umul_hi(x_lo, y));
    let hi = ((p >> 64) as u64).wrapping_add((lo < p_lo) as u64);
    (hi, lo)
}

/// High 64 bits of `x * y + c`.
#[inline(always)]
fn umul_add_hi(x: u64, y: u64, c: u64) -> u64 {
    (((x as u128 * y as u128) + c as u128) >> 64) as u64
}

// ---------------------------------------------------------------------------
// Decimal conversion
// ---------------------------------------------------------------------------

/// Fixed-width significand, plus the one digit past it that rounding may or
/// may not require.
struct Decimal {
    /// Fifteen or sixteen digits for `f64`, seven or eight for `f32`.
    sig: u64,
    exp: i32,
    last_digit: u32,
    /// Whether `last_digit` is needed for the value to round trip. False means
    /// `sig` already lands inside the rounding interval on its own, so the
    /// shortest form is `sig` with its trailing zeros dropped.
    has_last_digit: bool,
}

impl Decimal {
    /// The held-back digit, or zero when the significand round trips without
    /// it. Shifting `sig` up by a place has to consult this, since the vacated
    /// place is a real digit in one case and a trailing zero in the other.
    #[inline(always)]
    fn held_digit(&self) -> u64 {
        if self.has_last_digit {
            self.last_digit as u64
        } else {
            0
        }
    }
}

/// Rescale a subnormal's significand up to the fixed width.
///
/// A subnormal has no implicit bit, so the product can come out with fewer
/// digits than the width. Multiplying back up is a loop, but subnormals are
/// rare enough to keep it out of the callers' instruction stream entirely.
#[cold]
#[inline(never)]
fn rescale_subnormal(d: &Decimal, threshold: u64) -> Decimal {
    let mut sig = d.sig * 10 + d.held_digit();
    let mut exp = d.exp;
    while sig < threshold {
        sig *= 10;
        exp -= 1;
    }
    let last_digit = (sig % 10) as u32;
    Decimal {
        sig: sig / 10,
        exp,
        last_digit,
        has_last_digit: last_digit != 0,
    }
}

const EXP_OFFSET_F64: i32 = 1023 + 52;
const EXP_OFFSET_F32: i32 = 127 + 23;

/// A half plus a nudge, so the `f64` last-digit multiply rounds to nearest
/// without a second comparison.
const BIASED_HALF: u64 = (1u64 << 63) + 6;

/// The lopsided-interval case: the value is a power of two, so its lower
/// neighbor is half as far away as its upper one.
///
/// Shared by both widths. It is off the hot path by construction, since it
/// only fires when every mantissa bit is zero.
#[cold]
#[inline(never)]
fn to_decimal_irregular(bin_sig: u64, bin_exp: i32) -> Decimal {
    let dec_exp = compute_dec_exp(bin_exp, false);
    let shift = compute_exp_shift(bin_exp, dec_exp + 1) + EXTRA_SHIFT;
    // The observed range over every exponent of both widths. The upper bound
    // is the load-bearing one: it keeps the `half_ulp` shift below from going
    // negative, and it is also what holds `bin_sig << shift` inside the word.
    debug_assert!((4..=EXTRA_SHIFT + 1).contains(&shift));
    let (p10_hi, p10_lo) = pow10(-dec_exp - 1);

    let (p_hi, p_lo) = umul192_hi128(p10_hi, p10_lo, bin_sig << shift);
    let integral = p_hi >> EXTRA_SHIFT;
    let fractional = (p_hi << (64 - EXTRA_SHIFT)) | (p_lo >> EXTRA_SHIFT);

    let half_ulp = p10_hi >> (EXTRA_SHIFT + 1 - shift);
    let round_up = half_ulp > u64::MAX - fractional;
    let round_down = (half_ulp >> 1) > fractional;

    // Two candidates for the next digit: the one nearest the true value, and
    // the smallest that still lands inside the interval. The larger wins,
    // which is what keeps the result shortest when the interval is skewed.
    let nearest = umul_add_hi(fractional, 10, (1u64 << 63) - 1);
    let lowest = umul_add_hi(fractional.wrapping_sub(half_ulp >> 1), 10, u64::MAX);

    Decimal {
        sig: integral + round_up as u64,
        exp: dec_exp,
        last_digit: if nearest < lowest { lowest } else { nearest } as u32,
        has_last_digit: !(round_up | round_down),
    }
}

/// `f64` significand and exponent. `bin_sig` carries the implicit bit.
#[inline]
fn to_decimal_f64(bin_sig: u64, raw_exp: i32, regular: bool) -> Decimal {
    let bin_exp = raw_exp - EXP_OFFSET_F64;
    if !regular {
        return to_decimal_irregular(bin_sig, bin_exp);
    }

    let dec_exp = compute_dec_exp(bin_exp, true);
    let shift = EXP_SHIFTS[raw_exp as usize] as i32;
    debug_assert_eq!(shift, compute_exp_shift(bin_exp, dec_exp + 1) + EXTRA_SHIFT);
    debug_assert!((3..=EXTRA_SHIFT).contains(&shift));
    let (p10_hi, p10_lo) = pow10(-dec_exp - 1);

    let (p_hi, p_lo) = umul192_hi128(p10_hi, p10_lo, bin_sig << shift);
    let integral = p_hi >> EXTRA_SHIFT;
    let fractional = (p_hi << (64 - EXTRA_SHIFT)) | (p_lo >> EXTRA_SHIFT);

    // An even significand may sit exactly on the boundary and still round
    // back, which is what round-half-to-even means here.
    let even = 1 - (bin_sig & 1);
    let half_ulp = (p10_hi >> (EXTRA_SHIFT + 1 - shift)) + even;
    let round_up = fractional.wrapping_add(half_ulp) < fractional;
    let round_down = half_ulp > fractional;

    let mut last_digit = umul_add_hi(fractional, 10, BIASED_HALF);
    if fractional == 1u64 << 62 {
        // Dead centre between two digits. The bias above would round up; the
        // even neighbour is the one that round-trips.
        last_digit = 2;
    }

    Decimal {
        sig: integral + round_up as u64,
        exp: dec_exp,
        last_digit: last_digit as u32,
        has_last_digit: !(round_up | round_down),
    }
}

/// Guard bits for `f32`, where the whole product fits in 64 bits and there is
/// room to spare.
const F32_EXTRA_SHIFT: i32 = 34;
const F32_FRAC_MASK: u64 = (1u64 << F32_EXTRA_SHIFT) - 1;

/// `f32` significand and exponent. `bin_sig` carries the implicit bit.
#[inline]
fn to_decimal_f32(bin_sig: u32, raw_exp: i32, regular: bool) -> Decimal {
    let bin_exp = raw_exp - EXP_OFFSET_F32;
    if !regular {
        return to_decimal_irregular(bin_sig as u64, bin_exp);
    }

    let dec_exp = compute_dec_exp(bin_exp, true);
    let shift = compute_exp_shift(bin_exp, dec_exp + 1) + F32_EXTRA_SHIFT;
    debug_assert!((F32_EXTRA_SHIFT - 3..=F32_EXTRA_SHIFT).contains(&shift));
    // Only the top half of the power of ten is needed at this precision. The
    // `+ 1` rounds it up, so the product is never short.
    let p10_hi = pow10(-dec_exp - 1).0;

    let p = umul_hi(p10_hi + 1, (bin_sig as u64) << shift);
    let integral = p >> F32_EXTRA_SHIFT;
    let fractional = p & F32_FRAC_MASK;

    let even = 1 - (bin_sig as u64 & 1);
    let half_ulp = (p10_hi >> (65 - shift)) + even;
    let round_up = (fractional + half_ulp) >> F32_EXTRA_SHIFT != 0;
    let round_down = half_ulp > fractional;

    let prod = fractional * 10;
    let mut last_digit = prod >> F32_EXTRA_SHIFT;
    let rem = prod & F32_FRAC_MASK;
    let half = 1u64 << (F32_EXTRA_SHIFT - 1);
    last_digit += (rem > half || (rem == half && last_digit & 1 != 0)) as u64;

    Decimal {
        sig: integral + round_up as u64,
        exp: dec_exp,
        last_digit: last_digit as u32,
        has_last_digit: !(round_up | round_down),
    }
}

// ---------------------------------------------------------------------------
// Digit generation
// ---------------------------------------------------------------------------

/// `'0'` in all eight bytes.
const ZEROS: u64 = 0x3030_3030_3030_3030;

/// Split an eight-digit number into one digit per byte, most significant
/// first, and report where its trailing zeros begin.
///
/// Three multiply-add steps replace seven divisions. Each step divides every
/// the word at once: adding `q * (2^w - d)` to a field simultaneously
/// subtracts `d * q` from it and deposits `q` a field-width higher, so one
/// multiply-add halves the field width and doubles the field count.
#[inline]
fn to_bcd8(v: u32) -> (u64, usize) {
    /// `2^40 / 10000`, rounded up: divides an eight-digit value by 10000.
    const DIV10K_SIG: u64 = (1u64 << 40) / 10_000 + 1;
    const NEG10K: u64 = (1u64 << 32) - 10_000;
    /// `2^19 / 100`, rounded up: exact for the four-digit fields it sees.
    const DIV100_SIG: u64 = (1u64 << 19) / 100 + 1;
    const NEG100: u64 = (1u64 << 16) - 100;
    /// `2^10 / 10`, rounded up: exact for the two-digit fields it sees.
    const DIV10_SIG: u64 = (1u64 << 10) / 10 + 1;
    const NEG10: u64 = (1u64 << 8) - 10;

    let v = v as u64;
    debug_assert!(v < 100_000_000);

    // One 8-digit field becomes two 4-digit fields, 32 bits apart.
    let d4 = v + NEG10K * ((v * DIV10K_SIG) >> 40);
    // Two 4-digit fields become four 2-digit fields, 16 bits apart.
    let d2 = d4 + NEG100 * (((d4 * DIV100_SIG) >> 19) & 0x7f_0000_007f);
    // Four 2-digit fields become eight 1-digit fields, one byte apart.
    let d1 = d2 + NEG10 * (((d2 * DIV10_SIG) >> 10) & 0xf_000f_000f_000f);

    // The fields come out least significant first. Reversing puts the leading
    // digit in the lowest byte, which is the first byte in memory.
    let bcd = d1.swap_bytes();
    (bcd, nonzero_end(bcd))
}

/// One past the last nonzero digit of a word laid out by [`to_bcd8`], so a
/// position rather than a count: `01234500` answers six.
///
/// Trailing zeros are the high bytes after the reversal, so a leading-zeros
/// count locates them. An all-zero word answers zero on its own, since
/// `leading_zeros` is total in Rust.
#[inline(always)]
fn nonzero_end(bcd: u64) -> usize {
    (70 - (bcd << 1).leading_zeros() as usize) / 8
}

/// Lay a decimal out as ASCII digits with its scientific exponent.
///
/// `sig` spells `WIDTH` digits, zero padded on the left when `padded` is set.
/// Only [`Decimal::has_last_digit`] distinguishes a value that needs every
/// digit from one whose trailing zeros should go.
#[inline]
fn lay_out<const WIDTH: usize>(d: &Decimal, padded: bool, e10: i32) -> Digits {
    let mut buf = [0u8; DIGIT_SCRATCH];

    // Both halves are converted even when the low one is zero, which upstream
    // skips with an early return. A zero low half is common (every short exact
    // decimal has one), so the branch would be a real mispredict rather than a
    // reliable saving; six multiplies are the cheaper bet.
    let end = if WIDTH == 16 {
        let hi = (d.sig / 100_000_000) as u32;
        let lo = (d.sig % 100_000_000) as u32;
        let (bcd_hi, end_hi) = to_bcd8(hi);
        let (bcd_lo, end_lo) = to_bcd8(lo);
        buf[..8].copy_from_slice(&(bcd_hi + ZEROS).to_le_bytes());
        buf[8..16].copy_from_slice(&(bcd_lo + ZEROS).to_le_bytes());
        // A zero low half has no significant digits of its own, so the run
        // ends in the high half rather than at byte eight.
        if lo == 0 { end_hi } else { 8 + end_lo }
    } else {
        let (bcd, end) = to_bcd8(d.sig as u32);
        buf[..8].copy_from_slice(&(bcd + ZEROS).to_le_bytes());
        end
    };

    let start = padded as usize;
    let len = if d.has_last_digit {
        // Rounding can carry the held-back digit to ten, but only along with a
        // carry into `sig`, which is what clears `has_last_digit`.
        //
        // That exclusion holds with zero margin: at `bin_exp == 0` the minimum
        // `half_ulp` is exactly the largest value that would admit a ten, and
        // the one `fractional` in that window is unreachable only because the
        // inputs at that exponent are integers. `BIASED_HALF`, `EXTRA_SHIFT`
        // and the `half_ulp` shift cannot be adjusted without redoing that
        // argument; a ten here would write `':'` into the output.
        debug_assert!(d.last_digit < 10);
        buf[WIDTH] = b'0' + d.last_digit as u8;
        WIDTH + 1 - start
    } else {
        end - start
    };

    debug_assert!(len >= 1 && buf[start] != b'0');
    Digits {
        buf,
        start,
        len,
        e10,
    }
}

/// Shortest round-tripping digits for a positive, finite, normal-or-subnormal
/// `f64`. Zero must be handled by the caller.
pub(crate) fn digits_f64(mag: f64) -> Digits {
    /// `f64` significands land in `[10^14, 10^16)`; at or above this they
    /// occupy all sixteen digits and need no left pad.
    const THRESHOLD: u64 = 1_000_000_000_000_000;

    let bits = mag.to_bits();
    let raw_exp = ((bits >> 52) & 0x7ff) as i32;
    let bin_sig = bits & ((1u64 << 52) - 1);
    debug_assert!(raw_exp != 0x7ff && mag > 0.0);

    let d = if raw_exp == 0 {
        rescale_subnormal(&to_decimal_f64(bin_sig, 1, true), THRESHOLD)
    } else {
        to_decimal_f64(bin_sig | (1u64 << 52), raw_exp, bin_sig != 0)
    };

    // `sig` spells sixteen digits, so its leading digit sits fifteen places
    // above the one `d.exp` names, or fourteen when the top digit is the pad.
    let full_width = d.sig >= THRESHOLD;
    let e10 = d.exp + 15 + full_width as i32;
    lay_out::<16>(&d, !full_width, e10)
}

/// Shortest round-tripping digits for a positive, finite `f32`. Zero must be
/// handled by the caller.
pub(crate) fn digits_f32(mag: f32) -> Digits {
    /// `f32` significands land in `[10^6, 10^8)`; at or above this they occupy
    /// all eight digits and need no left pad.
    const THRESHOLD: u64 = 10_000_000;

    let bits = mag.to_bits();
    let raw_exp = ((bits >> 23) & 0xff) as i32;
    let bin_sig = bits & ((1u32 << 23) - 1);
    debug_assert!(raw_exp != 0xff && mag > 0.0);

    let mut d = if raw_exp == 0 {
        rescale_subnormal(&to_decimal_f32(bin_sig, 1, true), THRESHOLD)
    } else {
        to_decimal_f32(bin_sig | (1u32 << 23), raw_exp, bin_sig != 0)
    };

    // `sig` spells eight digits, so its leading digit sits seven places above
    // the one `d.exp` names, or six when the top digit is the pad.
    let full_width = d.sig >= THRESHOLD;
    let mut e10 = d.exp + 7 + full_width as i32;
    if d.sig < 1_000_000 {
        // A digit short even of the padded width. Shifting up restores it, and
        // there is nothing left to hold back. The vacated place is a trailing
        // zero unless the held-back digit fills it, which `lay_out` then trims.
        d.sig = d.sig * 10 + d.held_digit();
        d.has_last_digit = false;
        e10 -= 1;
    }
    lay_out::<8>(&d, !full_width, e10)
}

#[cfg(test)]
mod tests {
    use super::to_bcd8;

    /// The SWAR split over its whole domain, against a decimal reference.
    ///
    /// Only some of these inputs are reachable from a float, so the float
    /// tests do not cover the routine on their own. Under four seconds in
    /// release; far too slow for a debug run, hence the `ignore`.
    ///
    /// `cargo test --release -- --ignored to_bcd8_exhaustive`
    #[test]
    #[ignore = "several seconds: checks all 10^8 inputs"]
    fn to_bcd8_exhaustive() {
        for v in 0u32..100_000_000 {
            let (bcd, end) = to_bcd8(v);
            let want = format!("{v:08}");
            for (i, (got, wanted)) in bcd.to_le_bytes().iter().zip(want.bytes()).enumerate() {
                assert_eq!(*got, wanted - b'0', "v={v} byte {i}");
            }
            assert_eq!(end, want.trim_end_matches('0').len(), "v={v} end");
        }
    }

    /// The boundaries the exhaustive sweep would catch, cheaply enough to run
    /// on every build: an all-zero word, a full width, and every position a
    /// run of trailing zeros can start at.
    #[test]
    fn to_bcd8_boundaries() {
        for (v, digits, end) in [
            (0u32, "00000000", 0),
            (1, "00000001", 8),
            (10, "00000010", 7),
            (99_999_999, "99999999", 8),
            (10_000_000, "10000000", 1),
            (12_345_678, "12345678", 8),
            (90_000_000, "90000000", 1),
            (1_020_000, "01020000", 4),
        ] {
            let (bcd, got_end) = to_bcd8(v);
            let got: Vec<u8> = bcd.to_le_bytes().iter().map(|b| b + b'0').collect();
            assert_eq!(core::str::from_utf8(&got).unwrap(), digits, "v={v}");
            assert_eq!(got_end, end, "v={v}");
        }
    }
}
