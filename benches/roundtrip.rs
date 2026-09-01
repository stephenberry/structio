//! The same benchmark Glaze runs, over the same document.
//!
//! `tmp/bench_glaze` generates `tmp/bench.json` and reports Glaze's numbers on
//! it; this reads the identical bytes so the two are directly comparable.

use std::time::Instant;

/// Glaze prettifies three spaces to a level by default, and matching it is what
/// lets both sides be measured over identical output, and lets the bytes
/// themselves be compared.
#[derive(Clone, Copy)]
struct GlazePretty;

impl structio::Options for GlazePretty {
    const PRETTY: bool = true;
    const INDENT: usize = 3;
}

// Field names match the Glaze benchmark's exactly, so both sides parse and
// emit the same keys.
#[derive(Default)]
#[allow(non_snake_case)]
struct TestStruct {
    testStrings: Vec<String>,
    testUints: Vec<u64>,
    testDoubles: Vec<f64>,
    testInts: Vec<i64>,
    testBools: Vec<bool>,
}
structio::object!(TestStruct {
    testStrings,
    testUints,
    testDoubles,
    testInts,
    testBools
});

#[derive(Default)]
#[rustfmt::skip]
struct TestGenerator {
    a: Vec<TestStruct>, b: Vec<TestStruct>, c: Vec<TestStruct>, d: Vec<TestStruct>,
    e: Vec<TestStruct>, f: Vec<TestStruct>, g: Vec<TestStruct>, h: Vec<TestStruct>,
    i: Vec<TestStruct>, j: Vec<TestStruct>, k: Vec<TestStruct>, l: Vec<TestStruct>,
    m: Vec<TestStruct>, n: Vec<TestStruct>, o: Vec<TestStruct>, p: Vec<TestStruct>,
    q: Vec<TestStruct>, r: Vec<TestStruct>, s: Vec<TestStruct>, t: Vec<TestStruct>,
    u: Vec<TestStruct>, v: Vec<TestStruct>, w: Vec<TestStruct>, x: Vec<TestStruct>,
    y: Vec<TestStruct>, z: Vec<TestStruct>,
}
#[rustfmt::skip]
structio::object!(TestGenerator {
    a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t, u, v, w, x, y, z
});

fn run(label: &str, path: &str) {
    let json = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("could not read {path}: {e}  (run tmp/bench_glaze first)");
            return;
        }
    };
    // Match the iteration count Glaze's side uses, so both do the same work.
    let iters = (40_000_000 / json.len()).max(20);

    // Read into a value that is reused, so allocations amortize. This is what
    // the Glaze benchmark measures too.
    let mut dst = TestGenerator::default();
    structio::read_into(&mut dst, &json).expect("parse failed");
    let t0 = Instant::now();
    for _ in 0..iters {
        structio::read_into(&mut dst, &json).expect("parse failed");
    }
    let read_s = t0.elapsed().as_secs_f64();

    // Write into a buffer that keeps its capacity between iterations.
    let mut out = String::with_capacity(json.len() * 2);
    structio::write_into(&dst, &mut out);
    let t0 = Instant::now();
    for _ in 0..iters {
        structio::write_into(&dst, &mut out);
    }
    let write_s = t0.elapsed().as_secs_f64();

    // Lay the same bytes out again as text, with no type in the way. The
    // buffer keeps its capacity between iterations, as the write loop's does.
    let mut pretty = String::with_capacity(json.len() * 2);
    structio::prettify_into_with::<GlazePretty>(&json, &mut pretty).expect("prettify failed");
    let t0 = Instant::now();
    for _ in 0..iters {
        structio::prettify_into_with::<GlazePretty>(&json, &mut pretty).expect("prettify failed");
    }
    let pretty_s = t0.elapsed().as_secs_f64();

    // Glaze writes its own prettified form beside each document. Reading it back
    // turns "byte-identical to Glaze" from a claim into a check, and gives the
    // minifier a realistic input: laid-out text, timed over its own size.
    let glaze_pretty = std::fs::read_to_string(path.replace(".json", "_pretty.json")).ok();
    let laid_out = glaze_pretty.as_deref().unwrap_or(&pretty);

    let mut mini = String::with_capacity(json.len());
    structio::minify_into(laid_out, &mut mini).expect("minify failed");
    let t0 = Instant::now();
    for _ in 0..iters {
        structio::minify_into(laid_out, &mut mini).expect("minify failed");
    }
    let minify_s = t0.elapsed().as_secs_f64();

    let mb = json.len() as f64 * iters as f64 / 1_048_576.0;
    let laid_out_mb = laid_out.len() as f64 * iters as f64 / 1_048_576.0;
    println!(
        "{label:<9} {:7} B  read {:8.2} MB/s   write {:8.2} MB/s   pretty {:8.2} MB/s   minify {:8.2} MB/s  {}{}{}",
        json.len(),
        mb / read_s,
        mb / write_s,
        mb / pretty_s,
        laid_out_mb / minify_s,
        if out == json {
            ""
        } else {
            "(ROUNDTRIP DIFFERS) "
        },
        match &glaze_pretty {
            Some(want) if *want != pretty => "(PRETTY DIFFERS FROM GLAZE) ",
            _ => "",
        },
        // Taking Glaze's layout back out has to land on the document Glaze
        // wrote in the first place.
        if mini == json {
            ""
        } else {
            "(MINIFY DIFFERS) "
        }
    );
}

fn main() {
    println!("--- structio ---");
    for (label, path) in [
        ("mixed", "tmp/bench.json"),
        ("strings", "tmp/bench_strings.json"),
        ("uints", "tmp/bench_uints.json"),
        ("doubles", "tmp/bench_doubles.json"),
        ("ints", "tmp/bench_ints.json"),
        ("bools", "tmp/bench_bools.json"),
        ("nice-dbl", "tmp/bench_nice_doubles.json"),
    ] {
        run(label, path);
    }
}
