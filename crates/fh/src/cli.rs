//! Command-line surface: every flag, subcommand, and argument the `fh`
//! binary accepts.
//!
//! Kept apart from the runtime so the CLI contract can be read (and
//! extended) in one place without wading through connection handling and
//! log streaming. `clap` derives everything below from these definitions,
//! including the generated `--help` text and shell completions.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "fh",
    version,
    about = "Farhand client: offload build/test execution to remote agent"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) subcommand: Option<Subcommands>,

    #[arg(
        long,
        env = "FARHAND_HOST",
        help = "Agent address, host:port (required, or from config)"
    )]
    pub(crate) host: Option<String>,

    #[arg(
        long,
        env = "FARHAND_TOKEN",
        help = "Shared authentication token (required, or from config)"
    )]
    pub(crate) token: Option<String>,

    #[arg(long, default_value = ".", help = "Local directory to sync")]
    pub(crate) dir: PathBuf,

    #[arg(
        long,
        help = "Project name / workspace key (default: local dir basename)"
    )]
    pub(crate) name: Option<String>,

    #[arg(long, help = "Path to configuration file (default: ./.farhand.yaml)")]
    pub(crate) config: Option<PathBuf>,

    #[arg(long, help = "Allow connecting to an agent with no token configured")]
    pub(crate) insecure_skip_token: bool,

    #[arg(
        short,
        long,
        help = "Print detailed sync, timing, and telemetry statistics"
    )]
    pub(crate) verbose: bool,

    #[arg(short = 'o', long = "output", action = clap::ArgAction::Append, help = "Explicit path(s) to fetch back after a successful run (repeatable)")]
    pub(crate) output: Vec<String>,

    #[arg(long = "no-output", help = "Disable artifact retrieval for this run")]
    pub(crate) no_output: bool,

    #[arg(
        long = "out-dir",
        help = "Local directory to extract artifacts into (default: ./farhand-out)"
    )]
    pub(crate) out_dir: Option<PathBuf>,

    #[arg(long, help = "Force specific template by name")]
    pub(crate) template: Option<String>,

    #[arg(long, help = "Pin to agent with tag (multi-agent mode)")]
    pub(crate) agent_tag: Option<String>,

    #[arg(long, help = "Bypass lockfile dependency caching hooks")]
    pub(crate) no_cache: bool,

    #[arg(
        long,
        help = "Explicit branch name for workspace isolation (defaults to current git branch)"
    )]
    pub(crate) branch: Option<String>,

    #[arg(long, help = "Disable automatic git branch workspace scoping")]
    pub(crate) no_branch_scope: bool,

    #[arg(
        long = "log-level",
        default_value = "info",
        help = "Log level (trace, debug, info, warn, error)"
    )]
    pub(crate) log_level: String,

    #[arg(
        long = "log-format",
        default_value = "text",
        help = "Log format ('text' or 'json')"
    )]
    pub(crate) log_format: String,

    #[arg(
        long = "no-env",
        action = clap::ArgAction::SetTrue,
        help = "Disable forwarding local environment variables to the remote agent (forwarding is ON by default)"
    )]
    pub(crate) no_env: bool,

    #[arg(
        short = 'e',
        long = "env",
        action = clap::ArgAction::Append,
        help = "Explicit environment variable to pass to remote command, in KEY=VALUE or KEY format (repeatable)"
    )]
    pub(crate) env: Vec<String>,

    #[arg(
        long = "print-env",
        action = clap::ArgAction::SetTrue,
        help = "List the environment variable NAMES that would be forwarded to the agent, then exit (values are never shown)"
    )]
    pub(crate) print_env: bool,

    #[arg(
        short = 't',
        long = "tty",
        help = "Allocate a pseudo-terminal (PTY) on the remote agent for interactive commands"
    )]
    pub(crate) tty: bool,

    #[arg(
        short = 'L',
        long = "forward",
        action = clap::ArgAction::Append,
        help = "Forward local port to remote agent port, formatted LOCAL:REMOTE (e.g. 3000:3000, repeatable)"
    )]
    pub(crate) forward: Vec<String>,

    #[arg(
        long = "watch",
        help = "Watch local files and re-trigger remote build continuously on file change"
    )]
    pub(crate) watch: bool,

    /// Milliseconds to coalesce filesystem events over in watch mode.
    /// Raise it for editors that save in bursts or on network filesystems.
    #[arg(
        long = "watch-debounce",
        value_name = "MS",
        default_value_t = 150,
        help = "Milliseconds to coalesce file events over in watch mode"
    )]
    pub(crate) watch_debounce: u64,

    #[arg(
        long = "compression",
        help = "Wire compression algorithm ('zstd', 'gzip', or 'none')"
    )]
    pub(crate) compression: Option<String>,

    #[arg(
        long = "tls",
        action = clap::ArgAction::SetTrue,
        help = "Enable TLS encryption for connection to agent"
    )]
    pub(crate) tls: bool,

    #[arg(
        long = "tls-ca",
        help = "Path to custom CA certificate (PEM) to verify agent TLS certificate"
    )]
    pub(crate) tls_ca: Option<PathBuf>,

    #[arg(
        long = "tls-fingerprint",
        help = "Expected SHA-256 fingerprint of the agent TLS certificate"
    )]
    pub(crate) tls_fingerprint: Option<String>,

    #[arg(
        long = "tls-insecure",
        action = clap::ArgAction::SetTrue,
        help = "Accept any server TLS certificate without validation (INSECURE)"
    )]
    pub(crate) tls_insecure: bool,

    #[arg(
        long = "tls-cert",
        help = "Path to client TLS certificate (PEM) for mTLS authentication"
    )]
    pub(crate) tls_cert: Option<PathBuf>,

    #[arg(
        long = "tls-key",
        help = "Path to client TLS private key (PEM) for mTLS authentication"
    )]
    pub(crate) tls_key: Option<PathBuf>,

    #[arg(
        short = 'T',
        long = "toolchain",
        action = clap::ArgAction::Append,
        help = "Declarative toolchain version override, e.g. -T rust=1.78.0 -T node=20 (repeatable)"
    )]
    pub(crate) toolchain: Vec<String>,

    #[arg(trailing_var_arg = true, help = "Command to run remotely")]
    pub(crate) command: Vec<String>,
}

#[derive(Subcommand, Debug)]
pub(crate) enum Subcommands {
    /// Initialize Farhand project configuration (.farhand.yaml) and templates
    Init {
        /// Target project directory (default: current directory)
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Remote agent address (host:port)
        #[arg(long)]
        host: Option<String>,

        /// Remote authentication token
        #[arg(long)]
        token: Option<String>,

        /// Write the safe `token: "${FARHAND_TOKEN}"` interpolation instead of
        /// a plaintext token (recommended for committed configs)
        #[arg(long)]
        token_env: bool,

        /// Project name (default: directory name)
        #[arg(short, long)]
        name: Option<String>,

        /// Project template preset (rust, npm, go, python, maven, gradle, or custom)
        #[arg(short, long)]
        template: Option<String>,

        /// Also generate a project-level template in .farhand/templates/<name>.yaml
        #[arg(long)]
        with_template: bool,

        /// Overwrite existing configuration and template files if they exist
        #[arg(short, long)]
        force: bool,
    },
    /// Watch local files and continuously offload builds on change
    Watch {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Manage build and detection templates
    Templates {
        #[command(subcommand)]
        action: TemplateAction,
    },
    /// Query build and execution history from remote agent
    History {
        /// Project name to query (default: current project name)
        #[arg(long)]
        name: Option<String>,

        /// Maximum number of runs to display (default: 10)
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Bring the agent's workspace up to date without running a build
    Sync {
        /// Report what would be transferred without sending any file data
        #[arg(long)]
        dry_run: bool,

        /// List every path that would be transferred (implies --dry-run detail)
        #[arg(long)]
        list: bool,
    },

    /// Print a shell completion script to stdout
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Write man pages for `fh` into a directory
    Man {
        /// Directory to write the pages into
        #[arg(long, default_value = "man")]
        dir: PathBuf,
    },

    /// Diagnose the connection, agent capacity, and configuration in one pass
    Doctor,

    /// Explain what happens to a single path: uploaded, already on the agent, or ignored
    Why {
        /// Path to explain (relative to the project directory, or absolute)
        path: String,
    },

    /// Clean remote project workspaces or caches
    Clean {
        /// Specific project/branch name to clean (default: current project/branch)
        #[arg(long)]
        name: Option<String>,

        /// Clean all non-canonical branch workspaces for this project
        #[arg(long)]
        all_branches: bool,

        /// Only clean intermediate compiler/build caches (incremental caches, .cache)
        #[arg(long)]
        caches_only: bool,
    },
    /// Open an interactive shell inside the remote workspace
    Shell {
        /// Optional specific shell to launch (default: $SHELL or /bin/sh)
        #[arg(long)]
        shell: Option<String>,
        /// Do not sync local changes before opening shell
        #[arg(long)]
        no_sync: bool,
    },
    /// Run an ad-hoc command in the remote workspace without artifact sync or dependency hooks
    Exec {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
        /// Allocate pseudo-terminal (PTY) for interactive execution
        #[arg(short = 't', long = "tty")]
        tty: bool,
    },
    /// Monitor remote agent daemon activity and resource utilization
    Top {
        /// Remote agent address (host:port) to monitor
        #[arg(long)]
        agent: Option<String>,
        /// Print a single snapshot and exit instead of interactive dashboard
        #[arg(long)]
        once: bool,
        /// Update interval in seconds (default: 1)
        #[arg(short, long, default_value_t = 1)]
        interval: u64,
    },
    /// Inspect or manage remote agents
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
    /// Offload Language Server Protocol (LSP) server to remote agent workspace
    Lsp {
        /// Remote agent address (host:port)
        #[arg(long)]
        agent: Option<String>,
        /// Bypass initial project sync before starting language server
        #[arg(long)]
        no_sync: bool,
        /// Disable auto-syncing changed files on textDocument/didSave
        #[arg(long)]
        no_save_sync: bool,
        /// LSP server binary and arguments to run remotely
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum AgentAction {
    /// Display system specifications, resource usage, and active jobs for an agent
    Info {
        /// Remote agent address (host:port)
        #[arg(long)]
        agent: Option<String>,
        /// Output agent information as formatted JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum TemplateAction {
    /// List all available templates and their resolution sources
    List,
    /// Display the raw YAML definition of a template
    Show {
        /// Name of the template to display
        name: String,
    },
    /// Initialize a template in .farhand/templates/<name>.yaml
    Init {
        /// Name of the template to initialize
        name: String,
    },
    /// Upload a template to the remote agent daemon
    Push {
        /// Name of the template to upload
        name: String,
        /// Scope on agent host ("project" or "user")
        #[arg(long, default_value = "project")]
        scope: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn every_subcommand_documents_itself() {
        // A variant that lost its doc comment to a neighbour would otherwise
        // silently inherit the wrong description in `--help` and in the
        // generated man pages.
        let cmd = Cli::command();
        for sub in cmd.get_subcommands() {
            let name = sub.get_name();
            assert!(
                sub.get_about().is_some(),
                "subcommand `{name}` has no description"
            );
        }
    }

    #[test]
    fn no_two_subcommands_share_a_description() {
        // A misattached doc comment shows up as two commands carrying the same
        // text, which is the general shape of the bug this guards against.
        let cmd = Cli::command();
        let mut seen: Vec<(String, String)> = Vec::new();
        for sub in cmd.get_subcommands() {
            let about = sub.get_about().map(|a| a.to_string()).unwrap_or_default();
            if let Some((other, _)) = seen.iter().find(|(_, text)| *text == about) {
                panic!(
                    "`{other}` and `{}` share the description: {about}",
                    sub.get_name()
                );
            }
            seen.push((sub.get_name().to_string(), about));
        }
    }
}
