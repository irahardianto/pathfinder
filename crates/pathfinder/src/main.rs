//! Pathfinder — The Headless IDE MCP Server for AI Coding Agents.
//!
//! Entry point: parses CLI args, loads config, starts MCP server via stdio.

use anyhow::Context;
use clap::Parser;
use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::types::WorkspaceRoot;
use rmcp::ServiceExt;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

mod server;

use server::PathfinderServer;

/// Pathfinder — The Headless IDE MCP Server for AI Coding Agents.
#[derive(Parser, Debug)]
#[command(name = "pathfinder-mcp", version, about)]
struct Cli {
    /// Path to the workspace root directory.
    #[arg(value_name = "WORKSPACE_PATH")]
    workspace_path: PathBuf,

    /// Enable raw LSP JSON-RPC tracing to stderr (DEBUG level).
    #[arg(long, default_value_t = false)]
    lsp_trace: bool,
}

/// Run the Pathfinder MCP server.
///
/// Extracted from `main()` for testability. Accepts pre-parsed CLI args
/// so that tests can inject configurations without going through the CLI parser.
pub(crate) async fn run(
    workspace_path: PathBuf,
    lsp_trace: bool,
    worker_threads: usize,
    max_blocking_threads: usize,
) -> anyhow::Result<()> {
    let mut filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    if lsp_trace {
        if let Ok(dir) = "pathfinder_lsp::client::transport=debug".parse() {
            filter = filter.add_directive(dir);
        }
    }

    // Initialize tracing to stderr (stdout is used by MCP stdio transport)
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    tracing::info!(
        workspace = %workspace_path.display(),
        version = env!("CARGO_PKG_VERSION"),
        worker_threads = worker_threads,
        max_blocking_threads = max_blocking_threads,
        "Pathfinder starting"
    );

    // Validate workspace root
    let workspace_root = WorkspaceRoot::new(&workspace_path)
        .with_context(|| format!("Invalid workspace path: {}", workspace_path.display()))?;

    // Load configuration
    let config = PathfinderConfig::load(workspace_root.path())
        .await
        .with_context(|| "Failed to load configuration")?;

    // Create MCP server
    let server = PathfinderServer::new(workspace_root, config).await;

    tracing::info!("Starting MCP stdio transport");

    // Start MCP server via stdio
    let service = server
        .serve(rmcp::transport::io::stdio())
        .await
        .context("Failed to start MCP server")?;

    // Wait for the server to complete
    service.waiting().await?;

    tracing::info!("Pathfinder shutting down");
    Ok(())
}

fn main() -> anyhow::Result<()> {
    const MAX_BLOCKING_THREADS: usize = 64;

    let worker_threads =
        std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .max_blocking_threads(MAX_BLOCKING_THREADS)
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let cli = Cli::parse();
        run(
            cli.workspace_path,
            cli.lsp_trace,
            worker_threads,
            MAX_BLOCKING_THREADS,
        )
        .await
    })
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
