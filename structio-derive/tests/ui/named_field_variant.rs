#[derive(Default, structio::Structio)]
enum Threshold {
    #[default]
    Auto,
    Window { size: u32, guard: u32 },
}

fn main() {}
