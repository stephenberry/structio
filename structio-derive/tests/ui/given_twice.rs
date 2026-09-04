#[derive(Default, structio::Structio)]
struct Point {
    #[structio(rename = "a", rename = "b")]
    x: i32,
}

fn main() {}
