#[derive(Default, structio::Structio)]
#[structio(array)]
struct Vec3 {
    #[structio(rename = "x")]
    x: f64,
    y: f64,
    z: f64,
}

fn main() {}
