//! Protocol-level smoke tests for `maj mcp`: speaks newline-delimited
//! JSON-RPC 2.0 directly over the child's stdin/stdout, with no MCP SDK
//! client dependency — this suite IS the wire-contract test.
mod common; // fixture_catalog + asset_id_of

use base64::Engine as _;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

struct Mcp {
    child: Child,
    // `Option` (not a bare `ChildStdin`) so a test can close the write half
    // deliberately (see `close_stdin`) — the "client disconnects cleanly"
    // signal a real MCP client sends by ending its process — without
    // dropping the rest of `Mcp` (still needed to `wait()` on the child and
    // assert its exit status).
    stdin: Option<ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: i64,
    // The raw `initialize` response, kept around so a test can inspect the
    // capabilities the server advertised (see
    // `initialize_advertises_the_resources_capability`) without every test
    // having to replay its own handshake.
    initialize_response: serde_json::Value,
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
            stdin: Some(stdin),
            stdout,
            next_id: 0,
            initialize_response: serde_json::Value::Null,
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
        s.initialize_response = init;
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
        let stdin = self.stdin.as_mut().expect("stdin still open");
        writeln!(stdin, "{msg}").expect("write");
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
        let stdin = self.stdin.as_mut().expect("stdin still open");
        writeln!(stdin, "{msg}").expect("write");
    }

    #[cfg(test)]
    fn call_tool(&mut self, name: &str, args: &serde_json::Value) -> serde_json::Value {
        self.request(
            "tools/call",
            &serde_json::json!({"name": name, "arguments": args}),
        )
    }

    /// Closes the write half of the child's stdin — the clean-disconnect
    /// signal a real MCP client sends by ending its process, distinct from
    /// `Drop`'s hard `kill()`. Lets the server see EOF and run its own
    /// shutdown path instead of being killed out from under it.
    #[cfg(test)]
    fn close_stdin(&mut self) {
        self.stdin = None;
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
    assert_eq!(
        text, "tag_assets: not yet implemented over MCP — use the maj CLI",
        "stub text must be exact: {text}"
    );
}

/// The regression net for the critical fix in this suite's history: rmcp
/// 3.1.0's shutdown path calls `tokio::time::timeout` when the transport
/// closes, so a runtime built with only `.enable_io()` (no timer driver)
/// panics the worker thread on every clean client disconnect — a client
/// closing stdin at the end of a normal session, not a crash. This is
/// deliberately NOT a `kill()` (that's `Drop`'s job, and a killed process
/// never runs the shutdown path at all): closing just the write half of
/// stdin is what a real client does when its own process ends, and is the
/// only way to exercise the panicking path this test guards against.
#[test]
fn read_tool_then_clean_stdin_close_exits_success() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("list_volumes", &serde_json::json!({}));
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");

    mcp.close_stdin();
    let status = mcp.child.wait().expect("wait for the server to exit");
    assert!(
        status.success(),
        "a clean stdin close must exit 0, not panic: {status:?}"
    );
}

/// Plants a thumb blob at the real `BlobStore` path (`majestical-index` is
/// already a `[dependencies]` entry of this crate, not just a dev-dependency,
/// so the test can compute the exact same path the resource reader does
/// rather than hand-deriving `blobs/<hex[..2]>/<hex[2..]>/thumb-320.webp` and
/// risking drift from `BlobStore::path_for`'s real layout).
#[test]
fn thumb_resource_serves_webp_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let asset = common::asset_id_of(&root, &state, "a.txt");
    let hex = majestical_index::blob::asset_hex(&asset).expect("xxh3 asset id");
    let store = majestical_index::blob::BlobStore::new(&root);
    let thumb_path = store.path_for(hex, &majestical_index::blob::Derivation::Thumb);
    std::fs::create_dir_all(thumb_path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&thumb_path, b"RIFFfakewebp").expect("plant thumb blob");

    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.request(
        "resources/read",
        &serde_json::json!({"uri": format!("majestical://thumb/{asset}")}),
    );
    let contents = &resp["result"]["contents"][0];
    assert_eq!(
        contents["mimeType"],
        serde_json::json!("image/webp"),
        "{resp}"
    );
    let blob = contents["blob"].as_str().expect("base64 blob field");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(blob)
        .expect("valid base64");
    assert_eq!(decoded, b"RIFFfakewebp");
}

/// A resource error is a protocol-level JSON-RPC `error`, NOT a tool-style
/// `result.isError` — `read_resource` returns `Result<_, ErrorData>`, and
/// rmcp surfaces an `Err` there as the response's top-level `error` field
/// (see rmcp's own `test_resource_not_found_version.rs`), unlike every tool
/// call in this suite above, which nests its error inside a successful
/// `result`.
#[test]
fn missing_thumb_is_a_clean_resource_error_naming_the_remedy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let asset = common::asset_id_of(&root, &state, "a.txt");
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.request(
        "resources/read",
        &serde_json::json!({"uri": format!("majestical://thumb/{asset}")}),
    );
    assert!(resp["result"].is_null(), "must not be a success: {resp}");
    let message = resp["error"]["message"]
        .as_str()
        .expect("a protocol-level error");
    assert!(
        message.contains("maj index run --kinds thumbs"),
        "error must name the remedy: {message}"
    );
}

/// The keyframe manifest blob is plain JSON, NOT zstd-compressed — unlike
/// every other JSON derivation blob (transcripts, OCR, captions), which
/// `crates/services/src/index/run.rs` zstd-compresses before
/// `BlobStore::write_atomic`. `run_keyframe_items` writes
/// `keyframes_manifest_json`'s bytes straight through instead (see
/// `Derivation::KeyframeManifest`'s doc: "JSON list of keyframe
/// timestamps"), so this plants raw JSON, matching the real writer.
#[test]
fn keyframes_resource_serves_manifest_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let asset = common::asset_id_of(&root, &state, "a.txt");
    let hex = majestical_index::blob::asset_hex(&asset).expect("xxh3 asset id");
    let store = majestical_index::blob::BlobStore::new(&root);
    let manifest_path = store.path_for(
        hex,
        &majestical_index::blob::Derivation::KeyframeManifest {
            model_tag: majestical_index::model::MODEL_TAG,
        },
    );
    std::fs::create_dir_all(manifest_path.parent().expect("parent")).expect("mkdir");
    let manifest_json = br#"{"model_tag":"siglip2-b16-v1","detected":2,"timestamps":[1500,4500]}"#;
    std::fs::write(&manifest_path, manifest_json).expect("plant keyframe manifest");

    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.request(
        "resources/read",
        &serde_json::json!({"uri": format!("majestical://keyframes/{asset}")}),
    );
    let contents = &resp["result"]["contents"][0];
    assert_eq!(
        contents["mimeType"],
        serde_json::json!("application/json"),
        "{resp}"
    );
    let text = contents["text"].as_str().expect("text contents");
    let parsed: serde_json::Value = serde_json::from_str(text).expect("valid json");
    assert_eq!(
        parsed["timestamps"],
        serde_json::json!([1500, 4500]),
        "{text}"
    );
}

#[test]
fn resource_templates_are_listed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.request("resources/templates/list", &serde_json::json!({}));
    let templates = resp["result"]["resourceTemplates"]
        .as_array()
        .expect("resourceTemplates array");
    let mut uris: Vec<&str> = templates
        .iter()
        .map(|t| t["uriTemplate"].as_str().expect("uriTemplate"))
        .collect();
    uris.sort_unstable();
    assert_eq!(
        uris,
        vec![
            "majestical://keyframes/{asset_id}",
            "majestical://thumb/{asset_id}",
        ],
        "{templates:?}"
    );
    for template in templates {
        assert!(
            template["description"]
                .as_str()
                .is_some_and(|d| !d.is_empty()),
            "every template must describe what agents get: {template}"
        );
    }
}

/// Without `ensure_catalog`, resolving a blob path under a nonexistent
/// catalog root would still succeed as a plain path join (unlike the state
/// dir resolution `list_sync_locations`/`get_describer` guard against) —
/// but it would then read as a plain "no such file" `resource_not_found`,
/// not the `maj catalog init` remedy every other missing-catalog case in
/// this suite gives. This pins that the resource reader guards explicitly.
#[test]
fn resource_on_missing_catalog_names_remedy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("no-such-catalog");
    let state = dir.path().join("state");
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.request(
        "resources/read",
        &serde_json::json!({"uri": "majestical://thumb/xxh3:deadbeefdeadbeefdeadbeefdeadbeef"}),
    );
    let message = resp["error"]["message"]
        .as_str()
        .expect("a protocol-level error");
    assert!(
        message.contains("maj catalog init"),
        "error must name the remedy: {message}"
    );
}

/// `asset_hex` (`crates/index/src/blob.rs`) is the only thing standing
/// between a resource URI and a raw filesystem path join in
/// `BlobStore::path_for` — sabotage that removes its length/hex-alphabet
/// validation would let a payload like `../../../etc/passwd` ride through
/// as if it were a hex string, and neither resource had a test that would
/// have caught that before this one. Both `thumb` and `keyframes` share the
/// same `asset_hex` call, so both are pinned here.
#[test]
fn traversal_payload_in_asset_id_is_a_clean_error_for_both_resources() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);
    for kind in ["thumb", "keyframes"] {
        let resp = mcp.request(
            "resources/read",
            &serde_json::json!({
                "uri": format!("majestical://{kind}/xxh3:../../../etc/passwd")
            }),
        );
        assert!(
            resp["result"].is_null(),
            "a traversal payload must not be treated as a valid asset id: {resp}"
        );
        let message = resp["error"]["message"]
            .as_str()
            .expect("a protocol-level error");
        assert!(
            message.contains("not a valid asset id"),
            "{kind}: error must reject the malformed id, not the traversal path itself: {message}"
        );
    }
}

/// An id that isn't `xxh3:<hex>`-shaped at all (no prefix, or a prefix from
/// a hash algorithm this catalog doesn't use) must be rejected the same
/// clean way as a traversal payload, for both resources.
#[test]
fn malformed_non_xxh3_asset_id_is_a_clean_error_for_both_resources() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);
    for kind in ["thumb", "keyframes"] {
        let resp = mcp.request(
            "resources/read",
            &serde_json::json!({"uri": format!("majestical://{kind}/not-an-asset-id")}),
        );
        assert!(
            resp["result"].is_null(),
            "a non-xxh3 id must not be treated as valid: {resp}"
        );
        let message = resp["error"]["message"]
            .as_str()
            .expect("a protocol-level error");
        assert!(
            message.contains("not a valid asset id"),
            "{kind}: error must name the malformed id: {message}"
        );
    }
}

/// Pins the `.enable_resources()` call in `MajServer::get_info`
/// (`crates/cli/src/mcp_cmd/mod.rs`) — sabotage that drops it is otherwise
/// invisible to this suite, since every resource test above spawns its own
/// server and never inspects `initialize`'s advertised capabilities.
#[test]
fn initialize_advertises_the_resources_capability() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mcp = Mcp::spawn(&root, &state);
    assert!(
        mcp.initialize_response["result"]["capabilities"]["resources"].is_object(),
        "initialize must advertise the resources capability: {}",
        mcp.initialize_response
    );
}
