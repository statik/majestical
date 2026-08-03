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

/// Asserts a tool error's text names the `maj catalog init` remedy — shared
/// by every "no `FsApp`, but still missing-catalog-aware" tool check below.
#[cfg(test)]
fn assert_missing_catalog_remedy(resp: &serde_json::Value) {
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
        "error must name the remedy, not a raw path/OS error: {text}"
    );
}

/// `list_sync_locations` and `get_describer` never open an `FsApp` — they
/// read state-dir-relative config directly — so without their own guard
/// they'd surface a raw `canonicalize`/`os error 2` instead of the same
/// `maj catalog init` remedy every `FsApp::open`-based tool gives.
#[test]
fn list_sync_locations_on_missing_catalog_names_remedy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("no-such-catalog");
    let state = dir.path().join("state");
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("list_sync_locations", &serde_json::json!({}));
    assert_missing_catalog_remedy(&resp);
}

#[test]
fn get_describer_on_missing_catalog_names_remedy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("no-such-catalog");
    let state = dir.path().join("state");
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("get_describer", &serde_json::json!({}));
    assert_missing_catalog_remedy(&resp);
}

/// A sabotage check: an error's remedy text must survive as the FULL
/// Display chain (`{err:#}`), not just the top-level message (`{err}`) —
/// swapping the format spec in `tool_error` would still pass every other
/// test in this suite (they only check for a remedy substring), so this
/// test specifically manufactures a two-link chain (an outer "parsing
/// <path>" context wrapping a real TOML parse error) and asserts BOTH
/// links survive in the tool error text.
#[test]
fn tool_error_preserves_the_full_context_chain_not_just_the_top_message() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let nas = dir.path().join("nas");
    std::fs::create_dir_all(&nas).expect("mkdir");
    // `location add` creates a valid sync.toml first — simplest way to land
    // one at its real, hash-keyed state-dir path without reimplementing
    // `state_dir_for`'s own path derivation here.
    common::maj(&root, &state)
        .args(["sync", "location", "add", "nas"])
        .arg(&nas)
        .assert()
        .success();
    let sync_tomls = common::walkdir_find(&state, "sync.toml");
    assert_eq!(sync_tomls.len(), 1, "{sync_tomls:?}");
    std::fs::write(&sync_tomls[0], "not valid toml {{{\n").expect("corrupt sync.toml");

    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("list_sync_locations", &serde_json::json!({}));
    assert_eq!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(
        text.contains("parsing") && text.contains("sync.toml"),
        "outer context must survive: {text}"
    );
    assert!(
        text.contains("TOML parse error"),
        "root cause must survive, not just the outer context: {text}"
    );
}

#[test]
fn run_saved_search_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    common::maj(&root, &state)
        .args(["search", "a.txt", "--save", "picks"])
        .assert()
        .success();
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("run_saved_search", &serde_json::json!({"name": "picks"}));
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let structured = &resp["result"]["structuredContent"];
    assert_eq!(structured["count"], serde_json::json!(1), "{structured}");
    assert_eq!(
        structured["results"][0]["name"],
        serde_json::json!("a.txt"),
        "{structured}"
    );
}

#[test]
fn list_saved_searches_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    common::maj(&root, &state)
        .args(["search", "a.txt", "--save", "picks"])
        .assert()
        .success();
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("list_saved_searches", &serde_json::json!({}));
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let saved = resp["result"]["structuredContent"]["saved"]
        .as_array()
        .expect("saved array");
    assert_eq!(saved.len(), 1, "{saved:?}");
    assert_eq!(saved[0]["name"], serde_json::json!("picks"));
    assert_eq!(saved[0]["query"], serde_json::json!("a.txt"));
}

#[test]
fn list_sync_locations_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let nas = dir.path().join("nas");
    std::fs::create_dir_all(&nas).expect("mkdir");
    common::maj(&root, &state)
        .args(["sync", "location", "add", "nas"])
        .arg(&nas)
        .assert()
        .success();
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("list_sync_locations", &serde_json::json!({}));
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let structured = &resp["result"]["structuredContent"];
    assert_eq!(
        structured["readonly"],
        serde_json::json!(false),
        "{structured}"
    );
    let locations = structured["locations"].as_array().expect("locations array");
    assert_eq!(locations.len(), 1, "{locations:?}");
    assert_eq!(locations[0]["name"], serde_json::json!("nas"));
}

#[test]
fn get_describer_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);

    let unset = mcp.call_tool("get_describer", &serde_json::json!({}));
    assert_ne!(
        unset["result"]["isError"],
        serde_json::json!(true),
        "{unset}"
    );
    assert_eq!(
        unset["result"]["structuredContent"],
        serde_json::json!({"configured": false}),
        "{unset}"
    );

    common::maj(&root, &state)
        .args([
            "describer",
            "set",
            "--backend",
            "ollama",
            "--model",
            "llava",
        ])
        .assert()
        .success();
    let configured = mcp.call_tool("get_describer", &serde_json::json!({}));
    assert_ne!(
        configured["result"]["isError"],
        serde_json::json!(true),
        "{configured}"
    );
    let structured = &configured["result"]["structuredContent"];
    assert_eq!(
        structured["configured"],
        serde_json::json!(true),
        "{structured}"
    );
    assert_eq!(
        structured["describer"]["model"],
        serde_json::json!("llava"),
        "{structured}"
    );
}

#[test]
fn suggest_tags_review_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("suggest_tags_review", &serde_json::json!({}));
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    assert_eq!(
        resp["result"]["structuredContent"],
        serde_json::json!({"pending": []}),
        "a fixture with no caption-runner tag-suggestion blobs has nothing pending: {resp}"
    );
}

/// One representative mutating stub — Task 8 replaces the body, but Task
/// 6's contract (roster stable now, every stub errors uniformly) is pinned
/// here so a regression in the stub wiring itself is caught before Task 8.
#[test]
fn mutating_stub_tool_errors_as_not_yet_implemented() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("tag_assets", &serde_json::json!({}));
    assert_eq!(
        resp["result"]["isError"],
        serde_json::json!(true),
        "a stub mutating tool must report a tool error: {resp}"
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(
        text.contains("not yet implemented"),
        "stub text must say so: {text}"
    );
}
