use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "fh", about = "Farhand client: offload build/test execution to remote agent")]
struct Cli {
    #[arg(long, help = "Agent address, host:port")]
    host: Option<String>,

    #[arg(long, env = "FARHAND_TOKEN", help = "Shared authentication token")]
    token: Option<String>,

    #[arg(trailing_var_arg = true, help = "Command to run remotely")]
    command: Vec<String>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    println!("Farhand client initialized (Stage 00)");
    if let Some(host) = &cli.host {
        println!("Target host: {}", host);
    }
}
