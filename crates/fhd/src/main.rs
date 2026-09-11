use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "fhd", about = "Farhand daemon: persistent remote build agent")]
struct Cli {
    #[arg(long, default_value = "0.0.0.0:9876", help = "Address to listen on")]
    listen: String,

    #[arg(long, env = "FARHAND_TOKEN", help = "Shared authentication token")]
    token: Option<String>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    println!("Farhand agent daemon initialized (Stage 00)");
    println!("Listening on: {}", cli.listen);
}
