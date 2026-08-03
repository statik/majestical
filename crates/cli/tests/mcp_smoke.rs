//! Protocol-level smoke tests for `maj mcp`: speaks newline-delimited
//! JSON-RPC 2.0 directly over the child's stdin/stdout, with no MCP SDK
//! client dependency — this suite IS the wire-contract test.
mod common; // fixture_catalog + asset_id_of

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: i64,
}

// `#[cfg(test)]` on every method below is not redundant despite this whole
// file already building with `--cfg test`: this repo's `clippy.toml` sets
// `allow-expect-in-tests`, and clippy's in-test detection for that config
// keys off `#[test]`/`#[cfg(test)]` directly on the item, not the ambient
// test-binary cfg — see the same pattern (and its fuller rationale) on the
// helpers in `tests/common/mod.rs`.
impl Mcp {
    #[cfg(test)]
    fn spawn(catalog: &std::path::Path, state: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_maj"))
            .env("MAJ_CATALOG", catalog)
            .env("MAJ_MACHINE_ID", "m1")
            .env("MAJ_STATE_DIR", state)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn maj mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut s = Self {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        let init = s.request(
            "initialize",
            &serde_json::json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "mcp_smoke", "version": "0"}
            }),
        );
        assert!(
            init["result"]["serverInfo"]["name"].is_string(),
            "initialize must report a server name: {init}"
        );
        s.notify("notifications/initialized", &serde_json::json!({}));
        s
    }

    #[cfg(test)]
    fn request(&mut self, method: &str, params: &serde_json::Value) -> serde_json::Value {
        self.next_id += 1;
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "id": self.next_id,
            "method": method, "params": params
        });
        writeln!(self.stdin, "{msg}").expect("write");
        let mut line = String::new();
        loop {
            line.clear();
            self.stdout.read_line(&mut line).expect("read");
            assert!(!line.is_empty(), "server closed stdout before responding");
            let v: serde_json::Value = serde_json::from_str(&line).expect("json");
            if v["id"] == serde_json::json!(self.next_id) {
                return v;
            } // skip server notifications
        }
    }

    #[cfg(test)]
    fn notify(&mut self, method: &str, params: &serde_json::Value) {
        let msg = serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
        writeln!(self.stdin, "{msg}").expect("write");
    }

    #[cfg(test)]
    fn call_tool(&mut self, name: &str, args: &serde_json::Value) -> serde_json::Value {
        self.request(
            "tools/call",
            &serde_json::json!({"name": name, "arguments": args}),
        )
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The full read+mutating tool roster Task 6 pins: mutating tools register
/// now with a stub "not yet implemented" handler (Task 8 replaces the
/// bodies) so the roster itself is already stable.
const EXPECTED_TOOLS: &[&str] = &[
    "add_sync_location",
    "catalog_init",
    "get_asset",
    "get_describer",
    "index_run",
    "index_status",
    "ingest_source",
    "inbox_process",
    "list_saved_searches",
    "list_sync_locations",
    "list_volumes",
    "move_para",
    "rm_saved_search",
    "rm_sync_location",
    "run_saved_search",
    "scan_volume",
    "search_assets",
    "set_describer",
    "set_metadata",
    "suggest_tags_review",
    "sync_pull",
    "sync_push",
    "sync_status",
    "tag_assets",
    "test_describer",
    "verify_volume",
];

#[test]
fn tool_list_matches_roster() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.request("tools/list", &serde_json::json!({}));
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    let mut names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("tool name"))
        .collect();
    names.sort_unstable();
    let mut expected = EXPECTED_TOOLS.to_vec();
    expected.sort_unstable();
    assert_eq!(names, expected, "tool roster must match exactly");
}

#[test]
fn search_assets_rows_match_service_outcome() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("search_assets", &serde_json::json!({"query": "a.txt"}));
    assert_ne!(
        resp["result"]["isError"],
        serde_json::json!(true),
        "search_assets must not error on the fixture: {resp}"
    );
    let structured = &resp["result"]["structuredContent"];
    assert_eq!(structured["count"], serde_json::json!(1), "{structured}");
    let hit = &structured["results"][0];
    assert!(hit["asset"].is_string(), "{hit}");
    assert!(hit["name"].is_string(), "{hit}");
    assert_eq!(hit["known"], serde_json::json!(true), "{hit}");
    assert_eq!(hit["name"], serde_json::json!("a.txt"), "{hit}");
}

#[test]
fn get_asset_unknown_is_a_value_not_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);

    let unknown = mcp.call_tool(
        "get_asset",
        &serde_json::json!({"asset_id": "xxh3:deadbeefdeadbeefdeadbeefdeadbeef"}),
    );
    assert_ne!(
        unknown["result"]["isError"],
        serde_json::json!(true),
        "{unknown}"
    );
    assert_eq!(
        unknown["result"]["structuredContent"],
        serde_json::json!({"found": false}),
        "{unknown}"
    );

    let asset_id = common::asset_id_of(&root, &state, "a.txt");
    let known = mcp.call_tool("get_asset", &serde_json::json!({"asset_id": asset_id}));
    assert_ne!(
        known["result"]["isError"],
        serde_json::json!(true),
        "{known}"
    );
    let structured = &known["result"]["structuredContent"];
    assert_eq!(structured["found"], serde_json::json!(true), "{structured}");
    assert!(structured["asset"]["asset"].is_string(), "{structured}");
    assert!(
        structured["asset"]["tags"]
            .as_array()
            .expect("tags array")
            .iter()
            .any(|t| t == "demo"),
        "{structured}"
    );
}

#[test]
fn list_volumes_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("list_volumes", &serde_json::json!({}));
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let volumes = resp["result"]["structuredContent"]["volumes"]
        .as_array()
        .expect("volumes array");
    assert_eq!(volumes.len(), 1, "{volumes:?}");
    assert_eq!(volumes[0]["id"], serde_json::json!("vol1"));
    assert_eq!(volumes[0]["asset_count"], serde_json::json!(2));
}

#[test]
fn sync_status_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    // `sync::status` errors when no location is configured at all (see
    // `sync::resolve_targets`'s `NO_LOCATIONS_HINT`) — register one first so
    // this spot-checks the success shape, not that unrelated error path.
    let location = dir.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    common::maj(&root, &state)
        .args(["sync", "location", "add", "nas"])
        .arg(&location)
        .assert()
        .success();
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("sync_status", &serde_json::json!({}));
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let structured = &resp["result"]["structuredContent"];
    let rows = structured["rows"].as_array().expect("rows array");
    assert_eq!(rows.len(), 1, "{structured}");
    assert_eq!(
        structured["readonly"],
        serde_json::json!(false),
        "{structured}"
    );
}

#[test]
fn index_status_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("index_status", &serde_json::json!({}));
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let structured = &resp["result"]["structuredContent"];
    assert_eq!(
        structured["thumbs"]["pending"],
        serde_json::json!(0),
        "{structured}"
    );
    assert!(structured["failed_last_run"].is_object(), "{structured}");
}

#[test]
fn read_tool_on_missing_catalog_names_remedy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("no-such-catalog");
    let state = dir.path().join("state");
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("search_assets", &serde_json::json!({"query": "anything"}));
    assert_eq!(
        resp["result"]["isError"],
        serde_json::json!(true),
        "a missing catalog must be a tool error: {resp}"
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(
        text.contains("maj catalog init"),
        "error must name the remedy: {text}"
    );
}
