use clap::{Parser, Subcommand};
use lwiki::output::Envelope;
use serde_json::json;

#[derive(Parser)]
#[command(name = "lwiki", version, about)]
struct Arguments {
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true)]
    offline: bool,
    #[arg(long, global = true)]
    dry_run: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List only implemented capabilities.
    Capabilities,
    /// Emit a versioned public JSON schema.
    Schema { name: String },
}

fn main() {
    let args = match Arguments::try_parse() {
        Ok(args) => args,
        Err(error) => {
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                error.print().ok();
                return;
            }
            if std::env::args_os().any(|arg| arg == "--json") {
                let envelope = Envelope::failure("arguments", "USAGE", error.to_string());
                println!(
                    "{}",
                    serde_json::to_string(&envelope).expect("serializable envelope")
                );
            } else {
                eprint!("{error}");
            }
            std::process::exit(2);
        }
    };
    let (result, exit) = match args.command {
        Command::Capabilities => (
            Envelope::success(
                "capabilities",
                json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "commands": ["capabilities", "schema"],
                    "schemas": ["output", "record"],
                    "network": false
                }),
            ),
            0,
        ),
        Command::Schema { name } => {
            let text = match name.as_str() {
                "output" => Some(include_str!("../schemas/output-v1.json")),
                "record" => Some(include_str!("../schemas/record-v1.json")),
                _ => None,
            };
            match text {
                Some(text) => (
                    Envelope::success(
                        "schema",
                        serde_json::from_str(text).expect("bundled schema"),
                    ),
                    0,
                ),
                None => (
                    Envelope::failure(
                        "schema",
                        "CAPABILITY_UNAVAILABLE",
                        format!("Unknown schema: {name}"),
                    ),
                    6,
                ),
            }
        }
    };
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&result).expect("serializable envelope")
        );
    } else if result.ok {
        println!(
            "{}",
            serde_json::to_string_pretty(&result.data).expect("serializable data")
        );
    } else if let Some(error) = result.error {
        eprintln!("{}: {}", error.code, error.message);
    }
    std::process::exit(exit);
}
