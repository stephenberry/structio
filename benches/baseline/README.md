# Glaze baseline

The comparison benchmark. This generates the documents, writes them to `tmp/`, and reports Glaze's throughput on them; `benches/roundtrip.rs` then reads the same files, so both libraries are measured over identical bytes.

It also checks that Glaze's own round trip is byte-stable, which is what makes "structio's output is byte-identical to Glaze's" a meaningful claim. Alongside each document it leaves `<name>_pretty.json`, Glaze's own prettified form of it, so the Rust side can compare its prettifier's bytes rather than only its throughput. That file is also what the minifiers are timed over, and taking the layout back out has to land on the document Glaze wrote in the first place.

```sh
c++ -std=c++23 -O3 -DNDEBUG -march=native \
    -I /path/to/glaze/include \
    benches/baseline/glaze_baseline.cpp -o tmp/glaze_baseline
./tmp/glaze_baseline        # writes tmp/bench*.json
cargo bench --bench roundtrip
```

The struct layout mirrors Glaze's own `tests/json_performance/json_perf_benchmark.cpp`: 26 fields of vectors of a five-field struct holding vectors of strings, unsigned ints, doubles, signed ints, and bools. Single-type documents keep one vector kind populated so each converter can be read in isolation.
