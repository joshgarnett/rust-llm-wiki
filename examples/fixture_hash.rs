//! Development-only BLAKE3 helper for the deterministic synthetic fixture seed.
use std::io::{self, Read};

fn main() -> io::Result<()> {
    let mut bytes = Vec::new();
    io::stdin().read_to_end(&mut bytes)?;
    println!("blake3:{}", blake3::hash(&bytes).to_hex());
    Ok(())
}
