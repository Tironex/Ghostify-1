//! Tooling for the registry repository: key generation, signing and verification
//! of `index.json`. Runs in the registry's GitHub Actions workflow, never in the app.

use ghostify_market::index::Index;
use ghostify_market::signed::{self, SignedIndex, TrustedKey};
use std::fs;
use std::process::ExitCode;

const USAGE: &str = "usage:
  ghostify-registry keygen <secret-key-file>
  ghostify-registry sign <secret-key-file> <key-id> <index.json> <signed-index.json>
  ghostify-registry verify <signed-index.json> <key-id> <public-key> [min-sequence]";

fn run(args: &[String]) -> Result<String, String> {
    let err = |e: ghostify_core::Error| e.to_string();
    let io = |e: std::io::Error| e.to_string();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["keygen", file] => {
            if fs::metadata(file).is_ok() {
                return Err(format!(
                    "`{file}` already exists; refusing to overwrite a key"
                ));
            }
            let key = signed::generate_key().map_err(err)?;
            write_secret(file, &signed::encode_secret_key(&key)).map_err(io)?;
            Ok(format!("public key: {}", signed::encode_public_key(&key)))
        }
        ["sign", key_file, key_id, input, output] => {
            let key = signed::decode_secret_key(&fs::read_to_string(key_file).map_err(io)?)
                .map_err(err)?;
            let index: Index = serde_json::from_str(&fs::read_to_string(input).map_err(io)?)
                .map_err(|e| e.to_string())?;
            let signed = signed::sign(&index, key_id, &key).map_err(err)?;
            fs::write(
                output,
                serde_json::to_string(&signed).map_err(|e| e.to_string())?,
            )
            .map_err(io)?;
            Ok(format!(
                "signed {} listings, sequence {}",
                index.listings.len(),
                index.sequence
            ))
        }
        ["verify", file, key_id, public_key, rest @ ..] => {
            let min = match rest {
                [] => 0,
                [n] => n
                    .parse::<u64>()
                    .map_err(|_| "min-sequence must be a number".to_owned())?,
                _ => return Err(USAGE.into()),
            };
            let signed: SignedIndex = serde_json::from_str(&fs::read_to_string(file).map_err(io)?)
                .map_err(|e| e.to_string())?;
            let keys = [TrustedKey {
                key_id: (*key_id).into(),
                public_key: (*public_key).into(),
            }];
            let index = signed::verify(&signed, &keys, min).map_err(err)?;
            Ok(format!(
                "valid: {} listings, sequence {}",
                index.listings.len(),
                index.sequence
            ))
        }
        _ => Err(USAGE.into()),
    }
}

#[cfg(unix)]
fn write_secret(path: &str, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(text.as_bytes())
}

#[cfg(not(unix))]
fn write_secret(path: &str, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(text.as_bytes())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
