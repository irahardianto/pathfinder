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
#[command(name = "pathfinder", version, about)]
struct Cli {
    /// Path to the workspace root directory.
    #[arg(value_name = "WORKSPACE_PATH")]
    workspace_path: PathBuf,

    /// Enable raw LSP JSON-RPC tracing to stderr (DEBUG level).
    #[arg(long, default_value_t = false)]
    lsp_trace: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing to stderr (stdout is used by MCP stdio transport)
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    let cli = Cli::parse();

    tracing::info!(
        workspace = %cli.workspace_path.display(),
        version = env!("CARGO_PKG_VERSION"),
        "Pathfinder starting"
    );

    // Validate workspace root
    let workspace_root = WorkspaceRoot::new(&cli.workspace_path)
        .with_context(|| format!("Invalid workspace path: {}", cli.workspace_path.display()))?;

    // Load configuration
    let config = PathfinderConfig::load(workspace_root.path())
        .await
        .with_context(|| "Failed to load configuration")?;

    // Create MCP server
    let server = PathfinderServer::new(workspace_root, config).await;

    // If LSP tracing is requested, we could inject that config later when LSP is implemented.
    if cli.lsp_trace {
        tracing::info!("LSP tracing enabled via --lsp-trace");
    }

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
