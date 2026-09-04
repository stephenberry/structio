#[derive(Default, structio::Structio)]
struct Config {
    #[structio(skip_if = "Vec::is_empty")]
    tags: Vec<String>,
}

#[derive(Default, structio::Structio)]
#[structio(tag = "kind", content = "data")]
enum Fix {
    #[default]
    Valid,
}

fn main() {}
