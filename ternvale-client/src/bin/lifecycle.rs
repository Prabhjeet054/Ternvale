//! `ternvale-client` script: create, start, wait for agent, snapshot, stop.

use std::path::PathBuf;
use std::time::Duration;

use ternvale_client::lifecycle::{self, Lifecycle};

fn usage() -> ! {
    eprintln!("usage: lifecycle --socket PATH --name NAME --snapshot LABEL --config FILE");
    std::process::exit(2);
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut socket = std::env::var_os("TERNVALE_API_SOCK").map(PathBuf::from);
    let mut name = "lab".to_string();
    let mut snapshot = "boot".to_string();
    let mut config_path: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => {
                socket = Some(PathBuf::from(args.next().unwrap_or_else(|| usage())));
            }
            "--name" => name = args.next().unwrap_or_else(|| usage()),
            "--snapshot" => snapshot = args.next().unwrap_or_else(|| usage()),
            "--config" => {
                config_path = Some(PathBuf::from(args.next().unwrap_or_else(|| usage())));
            }
            "-h" | "--help" => usage(),
            other => {
                eprintln!("unknown argument {other}");
                usage();
            }
        }
    }
    let Some(socket) = socket else {
        eprintln!("set --socket or TERNVALE_API_SOCK");
        usage();
    };
    let Some(config_path) = config_path else {
        eprintln!("--config FILE is required");
        usage();
    };
    let config_toml = match std::fs::read_to_string(&config_path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("error: read {}: {error}", config_path.display());
            std::process::exit(1);
        }
    };
    println!(
        "lifecycle {name} on {} snapshot={snapshot}",
        socket.display()
    );
    if let Err(error) = lifecycle::run(&Lifecycle {
        socket,
        name,
        snapshot,
        config_toml,
        agent_timeout: Duration::from_secs(120),
    })
    .await
    {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
    println!("ok");
}
