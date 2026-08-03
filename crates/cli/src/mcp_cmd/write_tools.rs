//! The 16 mutating-tool stubs: register the roster's mutating-tool names now
//! (Task 6) so the roster is stable, without implementing any body — Task 8
//! replaces each `not_yet_implemented` call with the real operation and its
//! confirm gate.
use super::MajServer;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{tool, tool_router};

/// A stub tool's uniform response: registers the tool name in the roster
/// now (so the roster is stable) without implementing its body — that
/// arrives with Task 8's mutating tools and confirm gate.
fn not_yet_implemented(tool: &str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(format!(
        "{tool}: not yet implemented over MCP — use the maj CLI"
    ))])
}

#[tool_router(router = write_tool_router, vis = "pub(super)")]
impl MajServer {
    /// Adds a named sync location. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "stubs keep &self so Task 8's real bodies don't change the signature; the \
                  message itself needs no state"
    )]
    fn add_sync_location(&self) -> CallToolResult {
        not_yet_implemented("add_sync_location")
    }

    /// Initializes a new catalog directory. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "stubs keep &self so Task 8's real bodies don't change the signature; the \
                  message itself needs no state"
    )]
    fn catalog_init(&self) -> CallToolResult {
        not_yet_implemented("catalog_init")
    }

    /// Works the derivation queue. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "stubs keep &self so Task 8's real bodies don't change the signature; the \
                  message itself needs no state"
    )]
    fn index_run(&self) -> CallToolResult {
        not_yet_implemented("index_run")
    }

    /// Verified copy from a source directory into a PARA-routed
    /// destination. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "stubs keep &self so Task 8's real bodies don't change the signature; the \
                  message itself needs no state"
    )]
    fn ingest_source(&self) -> CallToolResult {
        not_yet_implemented("ingest_source")
    }

    /// Processes a shared inbox folder. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "stubs keep &self so Task 8's real bodies don't change the signature; the \
                  message itself needs no state"
    )]
    fn inbox_process(&self) -> CallToolResult {
        not_yet_implemented("inbox_process")
    }

    /// Renames or archives a PARA node. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "stubs keep &self so Task 8's real bodies don't change the signature; the \
                  message itself needs no state"
    )]
    fn move_para(&self) -> CallToolResult {
        not_yet_implemented("move_para")
    }

    /// Removes a saved search. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "stubs keep &self so Task 8's real bodies don't change the signature; the \
                  message itself needs no state"
    )]
    fn rm_saved_search(&self) -> CallToolResult {
        not_yet_implemented("rm_saved_search")
    }

    /// Removes a sync location. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "stubs keep &self so Task 8's real bodies don't change the signature; the \
                  message itself needs no state"
    )]
    fn rm_sync_location(&self) -> CallToolResult {
        not_yet_implemented("rm_sync_location")
    }

    /// Hashes a directory into the catalog as `AssetSeen` events. Not yet
    /// implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "stubs keep &self so Task 8's real bodies don't change the signature; the \
                  message itself needs no state"
    )]
    fn scan_volume(&self) -> CallToolResult {
        not_yet_implemented("scan_volume")
    }

    /// Configures the caption/tag-suggestion describer backend. Not yet
    /// implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "stubs keep &self so Task 8's real bodies don't change the signature; the \
                  message itself needs no state"
    )]
    fn set_describer(&self) -> CallToolResult {
        not_yet_implemented("set_describer")
    }

    /// Sets an LWW metadata field on an asset. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "stubs keep &self so Task 8's real bodies don't change the signature; the \
                  message itself needs no state"
    )]
    fn set_metadata(&self) -> CallToolResult {
        not_yet_implemented("set_metadata")
    }

    /// Confirms or rejects pending AI tag suggestions. Not yet implemented
    /// over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "stubs keep &self so Task 8's real bodies don't change the signature; the \
                  message itself needs no state"
    )]
    fn tag_assets(&self) -> CallToolResult {
        not_yet_implemented("tag_assets")
    }

    /// Fetches everything configured locations have that this catalog
    /// doesn't. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "stubs keep &self so Task 8's real bodies don't change the signature; the \
                  message itself needs no state"
    )]
    fn sync_pull(&self) -> CallToolResult {
        not_yet_implemented("sync_pull")
    }

    /// Replicates this catalog to configured locations. Not yet implemented
    /// over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "stubs keep &self so Task 8's real bodies don't change the signature; the \
                  message itself needs no state"
    )]
    fn sync_push(&self) -> CallToolResult {
        not_yet_implemented("sync_push")
    }

    /// Probes the configured describer backend's connectivity/capability.
    /// Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "stubs keep &self so Task 8's real bodies don't change the signature; the \
                  message itself needs no state"
    )]
    fn test_describer(&self) -> CallToolResult {
        not_yet_implemented("test_describer")
    }

    /// Re-verifies a destination against its ASC MHL history. Not yet
    /// implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "stubs keep &self so Task 8's real bodies don't change the signature; the \
                  message itself needs no state"
    )]
    fn verify_volume(&self) -> CallToolResult {
        not_yet_implemented("verify_volume")
    }
}
