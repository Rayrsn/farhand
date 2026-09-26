#![forbid(unsafe_code)]

pub mod clean;
pub mod client;
pub mod doctor;
pub mod envfilter;
pub mod history;
pub mod init;
pub mod lsp;
pub mod pool;
pub mod progress;
pub mod sync;
pub mod top;
pub mod watch;

pub use clean::clean_workspace;
pub use client::{connect_to_agent, parse_forward_spec, resolve_tls_config, split_server_host};
pub use history::{
    format_bytes, format_duration, query_history, render_history_table, truncate_utf8,
};
pub use init::{init_project, InitError, InitOptions, InitResult};
pub use lsp::run_lsp;
pub use pool::{probe_agent_status, select_best_agent, AgentScore};
pub use top::{run_agent_info, run_top};
pub use watch::should_ignore_path;
