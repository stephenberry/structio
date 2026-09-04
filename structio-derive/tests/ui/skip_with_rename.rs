#[derive(Default, structio::Structio)]
struct Point {
    #[structio(skip, rename = "x")]
    x: i32,
}

fn main() {}
