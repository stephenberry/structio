//! The README's quickstart, kept runnable so it cannot rot.
//!
//! `cargo run --example quickstart`

// docs:begin
use structio::{from_beve, from_str, to_beve, to_string};

#[derive(Default, Debug, PartialEq)]
struct Config {
    name: String,
    port: u16,
    hosts: Vec<String>,
}

structio::object!(Config { name, port, hosts });

fn main() -> Result<(), structio::Error> {
    let text = r#"{"name":"api","port":8080,"hosts":["a","b"]}"#;

    // JSON, straight into the struct.
    let config: Config = from_str(text)?;
    assert_eq!(config.port, 8080);
    assert_eq!(to_string(&config), text);

    // The same schema, as BEVE binary. No second declaration.
    let bytes = to_beve(&config);
    let same: Config = from_beve(&bytes)?;
    assert_eq!(same, config);

    Ok(())
}
// docs:end
