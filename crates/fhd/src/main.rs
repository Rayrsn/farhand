use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "fhd", about = "Farhand daemon: persistent remote build agent")]
struct Cli {
    #[arg(long, default_value = "0.0.0.0:9876", help = "Address to listen on")]
    listen: String,

    #[arg(long, env = "FARHAND_TOKEN", help = "Shared authentication token")]
    token: Option<String>,

    #[arg(long, help = "Custom shell invocation (e.g. '/bin/sh -c')")]
    shell: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let listener = TcpListener::bind(&cli.listen).await?;
    info!("Farhand daemon listening on {}", cli.listen);

    fhd::run_server(listener, cli.token, cli.shell).await?;
    Ok(())
}
