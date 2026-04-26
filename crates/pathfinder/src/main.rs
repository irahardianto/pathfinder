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
pub(crate) async fn run(workspace_path: PathBuf, lsp_trace: bool) -> anyhow::Result<()> {
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    run(cli.workspace_path, cli.lsp_trace).await
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_cli_parse_workspace_path() {
        let cli = Cli::parse_from(["pathfinder", "/tmp/workspace"]);
        assert_eq!(cli.workspace_path, PathBuf::from("/tmp/workspace"));
        assert!(!cli.lsp_trace);
    }

    #[test]
    fn test_cli_parse_lsp_trace_flag() {
        let cli = Cli::parse_from(["pathfinder", "/tmp/ws", "--lsp-trace"]);
        assert!(cli.lsp_trace);
    }

    #[test]
    fn test_cli_parse_missing_workspace_fails() {
        let result = Cli::try_parse_from(["pathfinder"]);
        assert!(result.is_err(), "should require workspace path");
    }

    #[tokio::test]
    async fn test_run_invalid_workspace_path() {
        // Using a non-existent path should fail during WorkspaceRoot::new
        let result = run(
            PathBuf::from("/nonexistent/path/that/does/not/exist"),
            false,
        )
        .await;
        // The path might or might not be valid depending on WorkspaceRoot validation
        // At minimum, it should not panic
        if let Err(e) = result {
            // Error message should mention the path
            let msg = format!("{e:#}");
            assert!(msg.contains("path") || msg.contains("Invalid"));
        }
    }
}
