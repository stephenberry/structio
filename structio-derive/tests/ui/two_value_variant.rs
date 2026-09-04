#[derive(Default, structio::Structio)]
enum Span {
    #[default]
    None,
    Range(u32, u32),
}

fn main() {}
