#[derive(Default, structio::Structio)]
#[structio(deny_unknown_fields)]
struct Point {
    #[structio(key = "x")]
    x: i32,
}

fn main() {}
