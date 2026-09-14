use clap::Parser;
use std::path::PathBuf;
use tokio::net::TcpListener;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "fhd", about = "Farhand daemon: persistent remote build agent")]
struct Cli {
    #[arg(long, default_value = "0.0.0.0:9876", help = "Address to listen on")]
    listen: String,

    #[arg(long, env = "FARHAND_TOKEN", help = "Shared authentication token")]
    token: Option<String>,

    #[arg(long, help = "Root directory for persistent workspaces")]
    workdir: Option<PathBuf>,

    #[arg(long, help = "Custom shell invocation (e.g. '/bin/sh -c')")]
    shell: Option<String>,

    #[arg(
        long = "max-concurrent-runs",
        alias = "max-runs",
        help = "Maximum parallel runs across projects (default: CPU count)"
    )]
    max_concurrent_runs: Option<usize>,

    #[arg(
        long = "tag",
        action = clap::ArgAction::Append,
        help = "Agent capability tags (repeatable, e.g. '--tag lan --tag gpu')"
    )]
    tags: Vec<String>,

    #[arg(
        long = "log-level",
        default_value = "info",
        help = "Log level (trace, debug, info, warn, error)"
    )]
    log_level: String,

    #[arg(
        long = "log-format",
        default_value = "text",
        help = "Log format ('text' or 'json')"
    )]
    log_format: String,
}

fn setup_tracing(level_str: &str, format_str: &str) {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level_str));
    let fmt_layer = tracing_subscriber::fmt::layer().with_target(false);

    if format_str == "json" {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer.json())
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .init();
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    setup_tracing(&cli.log_level, &cli.log_format);
    let listener = TcpListener::bind(&cli.listen).await?;
    let workdir = cli
        .workdir
        .unwrap_or_else(workspace::default_workspaces_dir);

    info!("Farhand daemon listening on {}", cli.listen);
    info!("Persistent workspaces root: {}", workdir.display());

    fhd::run_server(
        listener,
        cli.token,
        workdir,
        cli.shell,
        cli.max_concurrent_runs,
        cli.tags,
    )
    .await?;
    Ok(())
}
