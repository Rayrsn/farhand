use clap::Parser;
use std::path::Path;

#[derive(Parser, Debug)]
#[command(name = "fh", about = "Farhand client: offload build/test execution to remote agent")]
struct Cli {
    #[arg(long, help = "Agent address, host:port")]
    host: Option<String>,

    #[arg(long, env = "FARHAND_TOKEN", help = "Shared authentication token")]
    token: Option<String>,

    #[arg(long, default_value = ".", help = "Local directory to scan")]
    dir: String,

    #[arg(short, long, help = "Print detailed file scan summary")]
    verbose: bool,

    #[arg(trailing_var_arg = true, help = "Command to run remotely")]
    command: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let scan_path = Path::new(&cli.dir);

    println!("=== Farhand Client (fh) ===");
    println!("Scanning project directory: {}", scan_path.canonicalize()?.display());

    let files = fileset::scan(scan_path, &[])?;
    println!("Discovered {} tracked files (ignores applied)", files.len());

    if cli.verbose {
        println!("Tracked files:");
        for (rel_path, meta) in &files {
            println!("  [{:>8} bytes] sha256: {}... | {}", meta.size, &meta.hash[..12], rel_path);
        }
    }

    if let Some(host) = &cli.host {
        println!("Target agent host: {}", host);
    }

    if !cli.command.is_empty() {
        println!("Target remote command: {:?}", cli.command);
    }

    Ok(())
}
