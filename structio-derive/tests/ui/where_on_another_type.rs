#[derive(Default, structio::Structio)]
struct Page<T>
where
    Vec<T>: Clone,
{
    items: Vec<T>,
}

fn main() {}
