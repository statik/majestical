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
        Self::spawn_with_extra_env(catalog, state, &[])
    }

    /// Like [`Self::spawn`], plus extra environment variables on the child
    /// — used by the fake-model regression test below to point
    /// `MAJ_MODEL_DIR` at a planted, byte-exact-size model without touching
    /// every other test's env.
    #[cfg(test)]
    fn spawn_with_extra_env(
        catalog: &std::path::Path,
        state: &std::path::Path,
        extra_env: &[(&str, &str)],
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_maj"));
        command
            .env("MAJ_CATALOG", catalog)
            .env("MAJ_MACHINE_ID", "m1")
            .env("MAJ_STATE_DIR", state)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn maj mcp");
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

    // The other half of the notices contract: when nothing went wrong the
    // field is absent from the wire entirely, rather than riding along as an
    // empty array on every response. The query has to be FILTER-ONLY to pin
    // this — a term search consults the semantic layers, which legitimately
    // record "unavailable" notes on a machine with no models installed, so
    // the same assertion on the term search above would pass or fail by
    // accident of what the test machine happens to have fetched.
    let filtered = mcp.call_tool("search_assets", &serde_json::json!({"query": "tag:demo"}));
    let structured = &filtered["result"]["structuredContent"];
    assert_eq!(structured["count"], serde_json::json!(1), "{structured}");
    assert!(
        structured.get("notices").is_none(),
        "an uneventful search must not carry a notices field: {structured}"
    );
}

/// Appends a line the event-log reader can't parse to this machine's only
/// segment, so the next read of the log records the corrupt-line warning —
/// the cheapest deterministic notice source these tests have. Only the read
/// that first passes the corrupt line records it (the sqlite view remembers
/// how far it synced), so the call under test must be the session's first.
///
/// `#[cfg(test)]` for the same reason it's on `Mcp`'s methods above: clippy's
/// `allow-expect-in-tests` keys off the attribute, not the ambient cfg.
#[cfg(test)]
fn corrupt_the_event_log(root: &std::path::Path) {
    let machine_dir = root.join("events").join("test-machine");
    let segment = std::fs::read_dir(&machine_dir)
        .expect("machine events dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
        .expect("one events jsonl");
    let mut bytes = std::fs::read(&segment).expect("read segment");
    bytes.extend_from_slice(b"this is not json\n");
    std::fs::write(&segment, bytes).expect("re-write segment");
}

/// The notices contract end-to-end: a diagnostic that used to be stderr
/// (invisible to an MCP client, which never sees the server's stderr) now
/// rides the outcome struct. Deterministic trigger: a corrupt event-log
/// line. The query is filter-only, so no model needs to be installed for
/// this to be the notice that shows up.
#[test]
fn search_assets_surfaces_notices_in_structured_content() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    corrupt_the_event_log(&root);

    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("search_assets", &serde_json::json!({"query": "tag:none"}));
    assert_ne!(
        resp["result"]["isError"],
        serde_json::json!(true),
        "a corrupt line degrades the read, it does not fail the tool: {resp}"
    );
    let structured = &resp["result"]["structuredContent"];
    let notices = structured["notices"]
        .as_array()
        .unwrap_or_else(|| panic!("notices must reach the wire: {structured}"));
    assert!(
        notices.iter().any(|note| note
            .as_str()
            .is_some_and(|s| s.contains("corrupt event log line"))),
        "the corrupt-log warning must be one of them: {structured}"
    );
}

/// Regression test for the nested-tokio-runtime panic `search_assets`/
/// `run_saved_search` could hit on a real machine: once a user has `maj
/// model fetch`ed a real model and `maj index run` built an index, every
/// term search opens the local Lance vector store
/// (`open_semantic_index`/`open_text_semantic_index` in
/// `crates/services/src/search.rs`), which builds and enters its OWN tokio
/// runtime (`VectorStore`/`TextVectorStore::open_existing` in
/// `crates/index/src/vector_store.rs`) — panicking if the calling thread is
/// already inside one, as every MCP `#[tool]` handler's thread is. CI never
/// hits this because no real model is ever installed in these tests.
///
/// This reaches the exact vulnerable line WITHOUT downloading a real model:
/// `model_present_for` (`crates/index/src/model.rs`) checks only that each
/// file exists at its exact declared byte length, never content or hash, so
/// a zero-filled file the right size passes; `VectorStore`/
/// `TextVectorStore::open_existing` gate on nothing but `dir.is_dir()`
/// before building their runtime (`crates/index/src/vector_store.rs`), so a
/// plain empty `lance` directory is enough. Verified as a real mutation
/// test: with `read_tools::search_assets`/`run_saved_search` NOT routed
/// through `run_off_tokio_runtime` (i.e. reverting that part of this fix),
/// this test either panics the server (observed as the MCP child closing
/// stdout, which fails `Mcp::request`'s "server closed stdout before
/// responding" assertion) or hangs — confirmed via `timeout 30 cargo test
/// -p majestical-cli --test mcp_smoke -- --exact
/// search_with_a_planted_fake_model_does_not_panic_the_server`, which
/// killed the run at the 30s wall clock with no response ever received.
#[test]
#[cfg(unix)]
fn search_with_a_planted_fake_model_does_not_panic_the_server() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());

    // Materializes the per-catalog state dir (and `catalog.db`) as a side
    // effect, so its path — and the `lance` dir this test plants beside it
    // — can be found below. Neither `scan` nor `tag add` (both already run
    // by `fixture_catalog`) ever opens the sqlite view themselves.
    common::maj(&root, &state)
        .args(["volumes", "list"])
        .assert()
        .success();
    let catalog_dbs = common::walkdir_find(&state, "catalog.db");
    assert_eq!(catalog_dbs.len(), 1, "{catalog_dbs:?}");
    let catalog_state_dir = catalog_dbs[0].parent().expect("parent").to_path_buf();
    std::fs::create_dir_all(catalog_state_dir.join("lance")).expect("mkdir lance");

    // Fakes a MiniLM install at the exact byte sizes `model_present_for`
    // checks, no real weights downloaded.
    let model_dir = dir
        .path()
        .join("models")
        .join(majestical_index::model::MINILM.tag);
    std::fs::create_dir_all(&model_dir).expect("mkdir model dir");
    for file in majestical_index::model::MINILM.files {
        let f = std::fs::File::create(model_dir.join(file.name)).expect("create model file");
        f.set_len(file.bytes).expect("set_len");
    }

    let model_dir_root = dir.path().join("models");
    let mut mcp = Mcp::spawn_with_extra_env(
        &root,
        &state,
        &[(
            "MAJ_MODEL_DIR",
            model_dir_root.to_str().expect("utf8 model dir"),
        )],
    );
    let resp = mcp.call_tool("search_assets", &serde_json::json!({"query": "a.txt"}));
    assert_ne!(
        resp["result"]["isError"],
        serde_json::json!(true),
        "a search that reaches the vector store must not error, and must not have panicked or \
         hung the server: {resp}"
    );
    let structured = &resp["result"]["structuredContent"];
    assert_eq!(structured["count"], serde_json::json!(1), "{structured}");
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
    common::maj(&root, &state)
        .args(["meta", "set", &asset_id, "shot", "sunset"])
        .assert()
        .success();
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
    // Pins the wire shape: `AssetDetail::fields` (a `Vec<(String, String)>`
    // internally) must serialize as a JSON OBJECT, not an array-of-pairs —
    // a regression back to the tuple shape would still pass every assertion
    // above, so this checks the shape explicitly, not just the value.
    let fields = &structured["asset"]["fields"];
    assert!(
        fields.is_object(),
        "fields must serialize as a JSON object, not an array-of-pairs: {structured}"
    );
    assert_eq!(fields["shot"], serde_json::json!("sunset"), "{structured}");
    // The other half of the notices contract: nothing went wrong reading
    // this catalog, so the field is absent from the wire entirely rather
    // than riding along as an empty array on every response. `get_asset`
    // is the right place to pin it — it only reads the projection, so
    // unlike a term search it cannot pick up a note from whichever models
    // happen to be installed on the machine running the test.
    assert!(
        structured["asset"].get("notices").is_none(),
        "an uneventful read must not carry a notices field: {structured}"
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

/// The 16 mutating tools, distinct from `EXPECTED_TOOLS`'s full roster —
/// every one of these takes `confirm: bool`, checked below.
const MUTATING_TOOLS: &[&str] = &[
    "add_sync_location",
    "catalog_init",
    "index_run",
    "ingest_source",
    "inbox_process",
    "move_para",
    "rm_saved_search",
    "rm_sync_location",
    "scan_volume",
    "set_describer",
    "set_metadata",
    "sync_pull",
    "sync_push",
    "tag_assets",
    "test_describer",
    "verify_volume",
];

/// Every mutating tool's `inputSchema` must document the confirm gate the
/// same way (so an agent can discover the dry-run/execute contract from
/// `tools/list` alone) and must NOT list `confirm` as required (it
/// defaults to `false`) — replaces Task 6's stub tripwire now that every
/// mutating tool has a real body.
#[test]
fn every_mutating_tool_documents_the_confirm_gate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.request("tools/list", &serde_json::json!({}));
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    for name in MUTATING_TOOLS {
        let tool = tools
            .iter()
            .find(|t| t["name"] == serde_json::json!(*name))
            .unwrap_or_else(|| panic!("{name}: missing from tools/list: {resp}"));
        let description = tool["inputSchema"]["properties"]["confirm"]["description"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: inputSchema has no confirm property: {tool}"));
        assert!(
            description.contains("dry-run") && description.contains("executes"),
            "{name}: confirm's description must document the gate: {description}"
        );
        let required = tool["inputSchema"]["required"]
            .as_array()
            .is_some_and(|r| r.iter().any(|v| v == "confirm"));
        assert!(
            !required,
            "{name}: confirm must default to false, not be required: {tool}"
        );
    }
}

/// The four enum-shaped params must advertise their closed value set in
/// `tools/list`, so a client discovers the legal values instead of guessing
/// a string and finding out at call time. schemars renders a fieldless enum
/// as a `$ref` into the schema's own `$defs`, so the value set is asserted
/// there rather than inline on the property. A failure here means the
/// published schema shape changed (schemars' representation, or the order of
/// the values) — this is a tripwire, not a bug detector, so update it
/// deliberately once the new shape is the one intended.
#[test]
fn enum_params_publish_their_value_sets_in_the_schema() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.request("tools/list", &serde_json::json!({}));
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    for (tool_name, param, def, values) in [
        (
            "tag_assets",
            "op",
            "TagOp",
            &["add", "rm", "confirm_suggestion", "reject_suggestion"][..],
        ),
        ("move_para", "op", "ParaOp", &["add", "rename", "archive"]),
        ("ingest_source", "dedupe", "DedupeMode", &["skip", "copy"]),
        (
            "set_describer",
            "backend",
            "DescriberBackend",
            &["ollama", "lm-studio", "open-router"],
        ),
    ] {
        let tool = tools
            .iter()
            .find(|t| t["name"] == serde_json::json!(tool_name))
            .unwrap_or_else(|| panic!("{tool_name}: missing from tools/list: {resp}"));
        let schema = &tool["inputSchema"];
        assert_eq!(
            schema["properties"][param]["$ref"],
            serde_json::json!(format!("#/$defs/{def}")),
            "{tool_name}.{param} must reference its enum definition: {schema}"
        );
        let published = schema["$defs"][def]["enum"]
            .as_array()
            .unwrap_or_else(|| panic!("{tool_name}.{param}: no enum value set: {schema}"));
        let published: Vec<&str> = published
            .iter()
            .map(|v| v.as_str().expect("enum value is a string"))
            .collect();
        assert_eq!(
            published, values,
            "{tool_name}.{param} must publish exactly today's wire strings"
        );
    }
}

/// A typo'd enum value dies at the parameter-schema layer, before any tool
/// logic runs — the schema-level validation that replaces the old
/// hand-rolled `parse_*` bail. The failure must be visible to the client as
/// a tool error naming the legal values, never a structured dry-run success
/// the caller could mistake for a plan.
#[test]
fn tag_assets_rejects_unknown_op_at_the_parameter_layer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let asset = common::asset_id_of(&root, &state, "a.txt");
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool(
        "tag_assets",
        &serde_json::json!({"asset": asset, "op": "bogus", "confirm": false}),
    );
    assert_eq!(
        resp["result"]["isError"],
        serde_json::json!(true),
        "unknown op must fail: {resp}"
    );
    assert!(
        resp["result"]["structuredContent"].get("would").is_none(),
        "a rejected op must not return a dry-run plan: {resp}"
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("error text must reach the client: {resp}"));
    assert!(
        text.contains("failed to deserialize parameters") && text.contains("bogus"),
        "the error must name the rejected value and the layer that rejected it: {text}"
    );
    assert!(
        text.contains("confirm_suggestion"),
        "the error must name the legal values: {text}"
    );
}

/// A hand-built write-tool response — one with no outcome struct of its own
/// to carry a `notices` field — still hands its diagnostics to the client,
/// via `with_notices`. `tag_assets`'s dry run reads the projection through
/// the app whose sink collects the warning, so this exercises the fold on a
/// response assembled entirely from `json!`.
#[test]
fn tag_assets_dry_run_folds_notices_into_its_hand_built_response() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let asset = common::asset_id_of(&root, &state, "a.txt");
    corrupt_the_event_log(&root);

    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool(
        "tag_assets",
        &serde_json::json!({"asset": asset, "op": "add", "tag": "kf"}),
    );
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let structured = &resp["result"]["structuredContent"];
    let notices = structured["notices"]
        .as_array()
        .unwrap_or_else(|| panic!("notices must reach the wire: {structured}"));
    assert!(
        notices.iter().any(|note| note
            .as_str()
            .is_some_and(|s| s.contains("corrupt event log line"))),
        "the corrupt-log warning must be one of them: {structured}"
    );
}

/// `set_metadata`'s dry run is hand-built the same way `tag_assets`'s is,
/// and reads its current value straight off the app's own projection so the
/// sink that collected the warning is the one `with_notices` drains.
/// Routing that read through `meta::meta_get` instead would drain the sink
/// into a `MetaOutcome` this response discards, silently losing every
/// diagnostic — which is what this test pins.
#[test]
fn set_metadata_dry_run_folds_notices_into_its_hand_built_response() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let asset = common::asset_id_of(&root, &state, "a.txt");
    corrupt_the_event_log(&root);

    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool(
        "set_metadata",
        &serde_json::json!({"asset": asset, "field": "rating", "value": "5"}),
    );
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let structured = &resp["result"]["structuredContent"];
    let notices = structured["notices"]
        .as_array()
        .unwrap_or_else(|| panic!("notices must reach the wire: {structured}"));
    assert!(
        notices.iter().any(|note| note
            .as_str()
            .is_some_and(|s| s.contains("corrupt event log line"))),
        "the corrupt-log warning must be one of them: {structured}"
    );
}

/// `get_asset`'s unknown-id arm is the one read-tool response with no
/// outcome struct behind it: the asset that would have carried the notices
/// does not exist. The buffer still reaches the client rather than dying
/// with the per-call app.
#[test]
fn get_asset_not_found_folds_notices_into_its_hand_built_response() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    corrupt_the_event_log(&root);

    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool(
        "get_asset",
        &serde_json::json!({"asset_id": "xxh3:deadbeefdeadbeefdeadbeefdeadbeef"}),
    );
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let structured = &resp["result"]["structuredContent"];
    assert_eq!(
        structured["found"],
        serde_json::json!(false),
        "{structured}"
    );
    let notices = structured["notices"]
        .as_array()
        .unwrap_or_else(|| panic!("notices must reach the wire: {structured}"));
    assert!(
        notices.iter().any(|note| note
            .as_str()
            .is_some_and(|s| s.contains("corrupt event log line"))),
        "the corrupt-log warning must be one of them: {structured}"
    );
}

/// `list_saved_searches` has no service outcome struct to ride: the verb
/// returns a bare `Vec<SavedSearch>`, so the wire object is this module's own
/// `SavedSearchesResult`. Pins that its `notices` field is actually populated
/// from the per-call app rather than left at its default.
#[test]
fn list_saved_searches_folds_notices_into_its_local_result_struct() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    corrupt_the_event_log(&root);

    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("list_saved_searches", &serde_json::json!({}));
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let structured = &resp["result"]["structuredContent"];
    let notices = structured["notices"]
        .as_array()
        .unwrap_or_else(|| panic!("notices must reach the wire: {structured}"));
    assert!(
        notices.iter().any(|note| note
            .as_str()
            .is_some_and(|s| s.contains("corrupt event log line"))),
        "the corrupt-log warning must be one of them: {structured}"
    );
}

/// `get_asset`'s found arm carries its notices NESTED, on the `AssetDetail`
/// the service drained them into — `structuredContent.asset.notices`, not the
/// top level. Pinning the location keeps a reader from concluding the buffer
/// is dropped on this path just because nothing sits beside `found`.
#[test]
fn get_asset_found_carries_notices_nested_on_the_asset() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    // Resolved BEFORE the log is corrupted: `asset_id_of` runs its own `maj`,
    // and a read that passes the corrupt line first would consume the warning.
    let asset_id = common::asset_id_of(&root, &state, "a.txt");
    corrupt_the_event_log(&root);

    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("get_asset", &serde_json::json!({"asset_id": asset_id}));
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let structured = &resp["result"]["structuredContent"];
    assert_eq!(structured["found"], serde_json::json!(true), "{structured}");
    assert!(
        structured["notices"].is_null(),
        "the found arm carries them on the asset, not beside `found`: {structured}"
    );
    let notices = structured["asset"]["notices"]
        .as_array()
        .unwrap_or_else(|| panic!("notices must ride the asset: {structured}"));
    assert!(
        notices.iter().any(|note| note
            .as_str()
            .is_some_and(|s| s.contains("corrupt event log line"))),
        "the corrupt-log warning must be one of them: {structured}"
    );
}

/// `index_run`'s executed arm updates the on-disk failure marker AFTER the
/// pass returns, through a sink of its own. Those lines are appended to the
/// run outcome's existing `notices` rather than shipped as a second field —
/// pins that the append happens at all. The catalog is empty and no models
/// are installed, so every kind degrades to a no-op and the pass is quick.
#[test]
fn index_run_appends_the_failure_report_note_to_the_run_outcome() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    // Materializes the per-catalog state dir so the marker can be planted in
    // it — the same trick `search_with_a_planted_fake_model_does_not_panic_the_server`
    // uses to find that directory.
    common::maj(&root, &state)
        .args(["volumes", "list"])
        .assert()
        .success();
    let catalog_dbs = common::walkdir_find(&state, "catalog.db");
    assert_eq!(catalog_dbs.len(), 1, "{catalog_dbs:?}");
    let catalog_state_dir = catalog_dbs[0].parent().expect("parent");
    std::fs::write(catalog_state_dir.join("index-failures.json"), b"{ not json")
        .expect("plant an unparsable marker");

    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("index_run", &serde_json::json!({"confirm": true}));
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let structured = &resp["result"]["structuredContent"];
    let notices = structured["notices"]
        .as_array()
        .unwrap_or_else(|| panic!("notices must reach the wire: {structured}"));
    assert!(
        notices.iter().any(|note| note
            .as_str()
            .is_some_and(|s| s.contains("ignoring unparsable failure report"))),
        "the marker-update note must be folded in: {structured}"
    );
}

#[test]
fn tag_assets_defaults_to_dry_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let asset = common::asset_id_of(&root, &state, "a.txt");
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool(
        "tag_assets",
        &serde_json::json!({"asset": asset, "op": "add", "tag": "kf"}),
    );
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let structured = &resp["result"]["structuredContent"];
    assert_eq!(
        structured["executed"],
        serde_json::json!(false),
        "{structured}"
    );
    assert!(
        structured["would"]
            .as_str()
            .is_some_and(|s| s.contains("kf")),
        "{structured}"
    );

    let search = mcp.call_tool("search_assets", &serde_json::json!({"query": "tag:kf"}));
    assert_eq!(
        search["result"]["structuredContent"]["count"],
        serde_json::json!(0),
        "a dry run must not touch the catalog: {search}"
    );
}

/// The watchlist's "dry run over-promises" fix, for the `tag_assets` ops
/// that validate on execute (`add` here; `rm` and `confirm_suggestion`
/// likewise): a preview must fail on an unknown asset id exactly like
/// `confirm: true` would, never describe the write as achievable.
/// `reject_suggestion` is the exception — see
/// [`tag_assets_reject_suggestion_dry_run_succeeds_on_unknown_asset`].
#[test]
fn tag_assets_dry_run_fails_on_unknown_asset() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool(
        "tag_assets",
        &serde_json::json!({
            "asset": "xxh3:ffffffffffffffffffffffffffffffff",
            "op": "add",
            "tag": "kf",
            "confirm": false
        }),
    );
    assert_eq!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(text.contains("unknown asset"), "{text}");
}

/// The other half of the guard: `tags::reject` records the pair as given
/// without checking it against any current suggestion, so a rejection on an
/// unknown asset id succeeds — a harmless no-op line rather than a full
/// blob scan on every reject. Its preview must therefore NOT validate, or
/// the dry run would fail where `confirm: true` succeeds.
#[test]
fn tag_assets_reject_suggestion_dry_run_succeeds_on_unknown_asset() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool(
        "tag_assets",
        &serde_json::json!({
            "asset": "xxh3:ffffffffffffffffffffffffffffffff",
            "op": "reject_suggestion",
            "tags": ["kf"],
            "confirm": false
        }),
    );
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let structured = &resp["result"]["structuredContent"];
    assert_eq!(
        structured["executed"],
        serde_json::json!(false),
        "{structured}"
    );
    assert!(
        structured["would"]
            .as_str()
            .is_some_and(|s| s.contains("reject suggested tag(s)")),
        "the preview must still describe the rejection: {structured}"
    );
}

#[test]
fn tag_assets_confirm_executes_and_is_visible_to_cli() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let asset = common::asset_id_of(&root, &state, "a.txt");
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool(
        "tag_assets",
        &serde_json::json!({"asset": asset, "op": "add", "tag": "kf", "confirm": true}),
    );
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    assert_eq!(
        resp["result"]["structuredContent"]["executed"],
        serde_json::json!(true),
        "{resp}"
    );

    // A different machine id, same catalog: the tag must be visible through
    // a wholly separate `maj` process, not just this MCP session's own
    // in-memory state.
    let out = common::maj_as(&root, &state, "cli-checker")
        .args(["search", "tag:kf", "--json"])
        .output()
        .expect("run");
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(hits["count"], serde_json::json!(1), "{hits}");
}

#[test]
fn ingest_source_dry_run_returns_plan_and_copies_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    common::maj(&root, &state)
        .args(["para", "add", "project", "client-x"])
        .assert()
        .success();
    let source = dir.path().join("source");
    std::fs::create_dir_all(&source).expect("mkdir");
    std::fs::write(source.join("clip.mov"), b"hello").expect("write");
    let dest = dir.path().join("dest");
    std::fs::create_dir_all(&dest).expect("mkdir");

    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool(
        "ingest_source",
        &serde_json::json!({
            "source": source.to_str().expect("utf8"),
            "dest": [dest.to_str().expect("utf8")],
            "para": "project/client-x",
        }),
    );
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let structured = &resp["result"]["structuredContent"];
    assert_eq!(
        structured["executed"],
        serde_json::json!(false),
        "{structured}"
    );
    let files = structured["plan"]["files"].as_array().expect("files array");
    assert_eq!(files.len(), 1, "{structured}");
    assert_eq!(
        files[0]["rel"],
        serde_json::json!("clip.mov"),
        "{structured}"
    );

    assert!(
        std::fs::read_dir(&dest)
            .expect("read dest")
            .next()
            .is_none(),
        "a dry run must not copy anything into dest"
    );
}

/// Two locations sharing one catalog's blobs: an unreadable blob fails the
/// SAME copy for both, so both locations end up `Ran` with a non-empty
/// `failures` list — the shared "one bad blob" scenario, no need for two
/// separately-broken blobs. Mirrors `sync_smoke.rs`'s own permission-guard
/// pattern (including its skip-under-root escape hatch: some environments,
/// notably running as root, don't enforce a mode-000 file).
#[test]
#[cfg(unix)]
fn sync_push_partial_failure_keeps_rows_and_maps_polarity() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());

    let blob_path = root.join("blobs/ab/abcd/thumb-320.webp");
    std::fs::create_dir_all(blob_path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&blob_path, b"thumb-bytes").expect("write blob");
    std::fs::set_permissions(&blob_path, std::fs::Permissions::from_mode(0o000))
        .expect("chmod 000");
    if std::fs::read(&blob_path).is_ok() {
        std::fs::set_permissions(&blob_path, std::fs::Permissions::from_mode(0o644))
            .expect("restore perms");
        eprintln!("skipping: this environment does not enforce a mode-000 file (likely root)");
        return;
    }

    let loc_a = dir.path().join("loc-a");
    let loc_b = dir.path().join("loc-b");
    std::fs::create_dir_all(&loc_a).expect("mkdir");
    std::fs::create_dir_all(&loc_b).expect("mkdir");
    common::maj(&root, &state)
        .args(["sync", "location", "add", "loc-a"])
        .arg(&loc_a)
        .assert()
        .success();
    common::maj(&root, &state)
        .args(["sync", "location", "add", "loc-b"])
        .arg(&loc_b)
        .assert()
        .success();

    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool("sync_push", &serde_json::json!({"confirm": true}));

    // Restore permissions so the tempdir can be cleaned up regardless of
    // what the assertions below find.
    std::fs::set_permissions(&blob_path, std::fs::Permissions::from_mode(0o644))
        .expect("restore perms");

    assert_eq!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let structured = &resp["result"]["structuredContent"];
    assert_eq!(
        structured["executed"],
        serde_json::json!(true),
        "{structured}"
    );
    let rows = structured["rows"].as_array().expect("rows array");
    assert_eq!(
        rows.len(),
        2,
        "rows for BOTH locations must be present: {structured}"
    );
    for row in rows {
        let ran = &row["Ran"];
        assert!(
            ran.is_object(),
            "every location must have attempted to run: {row}"
        );
        let failures = ran["failures"].as_array().expect("failures array");
        assert!(
            !failures.is_empty(),
            "the shared unreadable blob must fail for every location: {row}"
        );
    }
}

#[test]
fn catalog_init_refuses_when_a_catalog_already_exists() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);

    let dry = mcp.call_tool("catalog_init", &serde_json::json!({}));
    assert_ne!(dry["result"]["isError"], serde_json::json!(true), "{dry}");
    assert_eq!(
        dry["result"]["structuredContent"]["already_initialized"],
        serde_json::json!(true),
        "{dry}"
    );

    let confirmed = mcp.call_tool("catalog_init", &serde_json::json!({"confirm": true}));
    assert_eq!(
        confirmed["result"]["isError"],
        serde_json::json!(true),
        "re-initializing an existing catalog must refuse: {confirmed}"
    );
    let text = confirmed["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(text.contains("already exists"), "{text}");
}

#[test]
fn verify_volume_on_a_tampered_dir_is_iserror_with_the_report_attached() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let target = dir.path().join("verify-me");
    std::fs::create_dir_all(&target).expect("mkdir");
    std::fs::write(target.join("a.mov"), b"hello").expect("write");
    // Establishes the initial ASC MHL generation this test then tampers
    // against — `maj verify` (unlike `verify_dir_op`'s own unit tests)
    // re-verifies against EXISTING history rather than creating it, so the
    // first generation is written directly here, same as
    // `crates/services/src/verify.rs`'s own tests do.
    let hash_list =
        majestical_ingest::mhl::hash_dir(&target, "2026-01-01T00:00:00Z").expect("hash_dir");
    majestical_ingest::mhl::write_generation(&target, &hash_list).expect("write_generation");
    std::fs::write(target.join("a.mov"), b"TAMPERED").expect("tamper");

    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool(
        "verify_volume",
        &serde_json::json!({"dir": target.to_str().expect("utf8"), "confirm": true}),
    );
    assert_eq!(
        resp["result"]["isError"],
        serde_json::json!(true),
        "an altered file must report isError, not a silent success: {resp}"
    );
    let structured = &resp["result"]["structuredContent"];
    assert_eq!(
        structured["altered"],
        serde_json::json!(["a.mov"]),
        "{structured}"
    );
    assert_eq!(
        structured["executed"],
        serde_json::json!(true),
        "{structured}"
    );
}

#[test]
fn index_run_single_pass_on_a_text_only_fixture() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);

    let dry = mcp.call_tool("index_run", &serde_json::json!({"kinds": ["thumbs"]}));
    assert_ne!(dry["result"]["isError"], serde_json::json!(true), "{dry}");
    assert_eq!(
        dry["result"]["structuredContent"]["executed"],
        serde_json::json!(false),
        "{dry}"
    );

    let resp = mcp.call_tool(
        "index_run",
        &serde_json::json!({"kinds": ["thumbs"], "confirm": true}),
    );
    assert_ne!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let structured = &resp["result"]["structuredContent"];
    assert_eq!(
        structured["executed"],
        serde_json::json!(true),
        "{structured}"
    );
    // a.txt/b.txt are plain text — `MediaKind::Other` has no derivation, so
    // a real pass over a text-only fixture writes nothing and fails
    // nothing.
    assert_eq!(
        structured["thumbs"]["written"],
        serde_json::json!(0),
        "{structured}"
    );
    assert!(
        structured["thumbs"]["failed"]
            .as_array()
            .expect("failed array")
            .is_empty(),
        "{structured}"
    );
}

#[test]
fn move_para_archive_dry_run_plans_then_confirm_moves() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    common::maj(&root, &state)
        .args(["para", "add", "project", "client-x"])
        .assert()
        .success();
    let materialized = dir.path().join("materialized");
    let node_dir = materialized.join("Projects").join("client-x");
    std::fs::create_dir_all(&node_dir).expect("mkdir");
    std::fs::write(node_dir.join("a.txt"), b"hello").expect("write");

    let mut mcp = Mcp::spawn(&root, &state);
    let dry = mcp.call_tool(
        "move_para",
        &serde_json::json!({
            "op": "archive",
            "node": "project/client-x",
            "roots": [materialized.to_str().expect("utf8")],
        }),
    );
    assert_ne!(dry["result"]["isError"], serde_json::json!(true), "{dry}");
    let structured = &dry["result"]["structuredContent"];
    assert_eq!(
        structured["executed"],
        serde_json::json!(false),
        "{structured}"
    );
    assert_eq!(
        structured["moves"][0]["status"],
        serde_json::json!("planned"),
        "{structured}"
    );
    assert!(node_dir.is_dir(), "a dry run must not move anything");

    let confirmed = mcp.call_tool(
        "move_para",
        &serde_json::json!({
            "op": "archive",
            "node": "project/client-x",
            "roots": [materialized.to_str().expect("utf8")],
            "confirm": true,
        }),
    );
    assert_ne!(
        confirmed["result"]["isError"],
        serde_json::json!(true),
        "{confirmed}"
    );
    let structured = &confirmed["result"]["structuredContent"];
    assert_eq!(
        structured["executed"],
        serde_json::json!(true),
        "{structured}"
    );
    assert_eq!(
        structured["moves"][0]["status"],
        serde_json::json!("moved"),
        "{structured}"
    );
    assert!(
        !node_dir.exists(),
        "confirm must actually move the directory"
    );
    assert!(
        materialized
            .join("Archives")
            .join("client-x")
            .join("a.txt")
            .is_file()
    );
}

#[test]
fn rm_saved_search_dry_run_then_confirm_removes_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    common::maj(&root, &state)
        .args(["search", "a.txt", "--save", "picks"])
        .assert()
        .success();

    let mut mcp = Mcp::spawn(&root, &state);
    let dry = mcp.call_tool("rm_saved_search", &serde_json::json!({"name": "picks"}));
    assert_ne!(dry["result"]["isError"], serde_json::json!(true), "{dry}");
    let structured = &dry["result"]["structuredContent"];
    assert_eq!(
        structured["exists"],
        serde_json::json!(true),
        "{structured}"
    );
    assert_eq!(
        structured["executed"],
        serde_json::json!(false),
        "{structured}"
    );

    let saved_before = mcp.call_tool("list_saved_searches", &serde_json::json!({}));
    assert_eq!(
        saved_before["result"]["structuredContent"]["saved"]
            .as_array()
            .expect("saved array")
            .len(),
        1,
        "a dry run must not remove it: {saved_before}"
    );

    let confirmed = mcp.call_tool(
        "rm_saved_search",
        &serde_json::json!({"name": "picks", "confirm": true}),
    );
    assert_ne!(
        confirmed["result"]["isError"],
        serde_json::json!(true),
        "{confirmed}"
    );
    assert_eq!(
        confirmed["result"]["structuredContent"]["executed"],
        serde_json::json!(true),
        "{confirmed}"
    );

    let saved_after = mcp.call_tool("list_saved_searches", &serde_json::json!({}));
    assert!(
        saved_after["result"]["structuredContent"]["saved"]
            .as_array()
            .expect("saved array")
            .is_empty(),
        "{saved_after}"
    );
}

/// Closes the cargo-mutants gap on `add_sync_location_result`'s
/// `Ok(Default::default())`/`delete !`/`==`->`!=` survivors and the
/// `MajServer::add_sync_location` wrapper survivor: this tool had no
/// functional test before, only the roster/schema checks.
#[test]
fn add_sync_location_dry_run_then_confirm_is_visible_via_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let nas = dir.path().join("nas2");
    std::fs::create_dir_all(&nas).expect("mkdir");

    let mut mcp = Mcp::spawn(&root, &state);
    let dry = mcp.call_tool(
        "add_sync_location",
        &serde_json::json!({"name": "nas2", "path": nas.to_str().expect("utf8")}),
    );
    assert_ne!(dry["result"]["isError"], serde_json::json!(true), "{dry}");
    let structured = &dry["result"]["structuredContent"];
    assert_eq!(
        structured["already_configured"],
        serde_json::json!(false),
        "{structured}"
    );
    assert_eq!(
        structured["path_accessible"],
        serde_json::json!(true),
        "{structured}"
    );
    assert_eq!(
        structured["executed"],
        serde_json::json!(false),
        "{structured}"
    );

    let before = mcp.call_tool("list_sync_locations", &serde_json::json!({}));
    assert!(
        before["result"]["structuredContent"]["locations"]
            .as_array()
            .expect("locations array")
            .is_empty(),
        "a dry run must not add it: {before}"
    );

    let confirmed = mcp.call_tool(
        "add_sync_location",
        &serde_json::json!({
            "name": "nas2", "path": nas.to_str().expect("utf8"), "confirm": true
        }),
    );
    assert_ne!(
        confirmed["result"]["isError"],
        serde_json::json!(true),
        "{confirmed}"
    );
    assert_eq!(
        confirmed["result"]["structuredContent"]["executed"],
        serde_json::json!(true),
        "{confirmed}"
    );

    let after = mcp.call_tool("list_sync_locations", &serde_json::json!({}));
    let locations = after["result"]["structuredContent"]["locations"]
        .as_array()
        .expect("locations array");
    assert_eq!(locations.len(), 1, "{locations:?}");
    assert_eq!(
        locations[0]["name"],
        serde_json::json!("nas2"),
        "{locations:?}"
    );
}

/// Closes the cargo-mutants gap on `rm_sync_location_result`'s
/// `Ok(Default::default())`/`delete !`/`==`->`!=` survivors and the
/// `MajServer::rm_sync_location` wrapper survivor.
#[test]
fn rm_sync_location_dry_run_then_confirm_is_gone_via_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let nas = dir.path().join("nas3");
    std::fs::create_dir_all(&nas).expect("mkdir");
    common::maj(&root, &state)
        .args(["sync", "location", "add", "nas3"])
        .arg(&nas)
        .assert()
        .success();

    let mut mcp = Mcp::spawn(&root, &state);
    let dry = mcp.call_tool("rm_sync_location", &serde_json::json!({"name": "nas3"}));
    assert_ne!(dry["result"]["isError"], serde_json::json!(true), "{dry}");
    let structured = &dry["result"]["structuredContent"];
    assert_eq!(
        structured["configured"],
        serde_json::json!(true),
        "{structured}"
    );
    assert_eq!(
        structured["executed"],
        serde_json::json!(false),
        "{structured}"
    );

    let before = mcp.call_tool("list_sync_locations", &serde_json::json!({}));
    assert_eq!(
        before["result"]["structuredContent"]["locations"]
            .as_array()
            .expect("locations array")
            .len(),
        1,
        "a dry run must not remove it: {before}"
    );

    let confirmed = mcp.call_tool(
        "rm_sync_location",
        &serde_json::json!({"name": "nas3", "confirm": true}),
    );
    assert_ne!(
        confirmed["result"]["isError"],
        serde_json::json!(true),
        "{confirmed}"
    );
    assert_eq!(
        confirmed["result"]["structuredContent"]["executed"],
        serde_json::json!(true),
        "{confirmed}"
    );

    let after = mcp.call_tool("list_sync_locations", &serde_json::json!({}));
    assert!(
        after["result"]["structuredContent"]["locations"]
            .as_array()
            .expect("locations array")
            .is_empty(),
        "{after}"
    );
}

/// Closes the cargo-mutants gap on `scan_volume_result`'s
/// `Ok(Default::default())`/`delete !` survivors and the
/// `MajServer::scan_volume` wrapper survivor.
#[test]
fn scan_volume_dry_run_then_confirm_makes_the_file_searchable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let scan_dir = dir.path().join("to-scan");
    std::fs::create_dir_all(&scan_dir).expect("mkdir");
    std::fs::write(scan_dir.join("scanme.bin"), b"scan-bytes").expect("write");

    let mut mcp = Mcp::spawn(&root, &state);
    let dry = mcp.call_tool(
        "scan_volume",
        &serde_json::json!({"dir": scan_dir.to_str().expect("utf8"), "volume": "scanvol"}),
    );
    assert_ne!(dry["result"]["isError"], serde_json::json!(true), "{dry}");
    let structured = &dry["result"]["structuredContent"];
    assert_eq!(
        structured["would_scan_files"],
        serde_json::json!(1),
        "{structured}"
    );
    assert_eq!(
        structured["resolved_volume_id"],
        serde_json::json!("scanvol"),
        "{structured}"
    );
    assert_eq!(
        structured["executed"],
        serde_json::json!(false),
        "{structured}"
    );

    let confirmed = mcp.call_tool(
        "scan_volume",
        &serde_json::json!({
            "dir": scan_dir.to_str().expect("utf8"), "volume": "scanvol", "confirm": true
        }),
    );
    assert_ne!(
        confirmed["result"]["isError"],
        serde_json::json!(true),
        "{confirmed}"
    );
    let structured = &confirmed["result"]["structuredContent"];
    assert_eq!(structured["assets"], serde_json::json!(1), "{structured}");
    assert_eq!(
        structured["volume_id"],
        serde_json::json!("scanvol"),
        "{structured}"
    );

    let found = mcp.call_tool("search_assets", &serde_json::json!({"query": "scanme.bin"}));
    assert_ne!(
        found["result"]["isError"],
        serde_json::json!(true),
        "{found}"
    );
    let hit = &found["result"]["structuredContent"]["results"][0];
    assert_eq!(hit["name"], serde_json::json!("scanme.bin"), "{hit}");
}

/// Closes the cargo-mutants gap on `set_metadata_result`'s
/// `Ok(Default::default())`/`delete !` survivors and the
/// `MajServer::set_metadata` wrapper survivor. Uses a known asset
/// throughout; the unknown-id half is
/// [`set_metadata_dry_run_fails_on_unknown_asset`].
#[test]
fn set_metadata_dry_run_then_confirm_is_visible_via_get_asset() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let asset = common::asset_id_of(&root, &state, "a.txt");

    let mut mcp = Mcp::spawn(&root, &state);
    let dry = mcp.call_tool(
        "set_metadata",
        &serde_json::json!({"asset": asset, "field": "rating", "value": "5"}),
    );
    assert_ne!(dry["result"]["isError"], serde_json::json!(true), "{dry}");
    let structured = &dry["result"]["structuredContent"];
    assert_eq!(
        structured["current_value"],
        serde_json::json!(null),
        "{structured}"
    );
    assert_eq!(
        structured["executed"],
        serde_json::json!(false),
        "{structured}"
    );

    let confirmed = mcp.call_tool(
        "set_metadata",
        &serde_json::json!({
            "asset": asset, "field": "rating", "value": "5", "confirm": true
        }),
    );
    assert_ne!(
        confirmed["result"]["isError"],
        serde_json::json!(true),
        "{confirmed}"
    );
    assert_eq!(
        confirmed["result"]["structuredContent"]["executed"],
        serde_json::json!(true),
        "{confirmed}"
    );

    let known = mcp.call_tool("get_asset", &serde_json::json!({"asset_id": asset}));
    let fields = &known["result"]["structuredContent"]["asset"]["fields"];
    assert_eq!(fields["rating"], serde_json::json!("5"), "{fields}");
}

/// The watchlist's "dry run over-promises" fix: `meta_set` validates the
/// asset on execute, so its preview must fail on an unknown asset id
/// exactly like `confirm: true` would, never describe the write as
/// achievable.
#[test]
fn set_metadata_dry_run_fails_on_unknown_asset() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root, &state);
    let resp = mcp.call_tool(
        "set_metadata",
        &serde_json::json!({
            "asset": "xxh3:ffffffffffffffffffffffffffffffff",
            "field": "rating",
            "value": "5",
            "confirm": false
        }),
    );
    assert_eq!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(text.contains("unknown asset"), "{text}");
}

/// Closes the cargo-mutants gap on `set_describer_result`'s
/// `Ok(Default::default())`/`delete !` survivors and the
/// `MajServer::set_describer` wrapper survivor.
#[test]
fn set_describer_dry_run_then_confirm_is_visible_via_get_describer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());

    let mut mcp = Mcp::spawn(&root, &state);
    let dry = mcp.call_tool(
        "set_describer",
        &serde_json::json!({"backend": "ollama", "model": "llava"}),
    );
    assert_ne!(dry["result"]["isError"], serde_json::json!(true), "{dry}");
    let structured = &dry["result"]["structuredContent"];
    assert_eq!(
        structured["current"],
        serde_json::json!(null),
        "{structured}"
    );
    assert_eq!(
        structured["executed"],
        serde_json::json!(false),
        "{structured}"
    );

    let confirmed = mcp.call_tool(
        "set_describer",
        &serde_json::json!({"backend": "ollama", "model": "llava", "confirm": true}),
    );
    assert_ne!(
        confirmed["result"]["isError"],
        serde_json::json!(true),
        "{confirmed}"
    );
    assert_eq!(
        confirmed["result"]["structuredContent"]["executed"],
        serde_json::json!(true),
        "{confirmed}"
    );

    let described = mcp.call_tool("get_describer", &serde_json::json!({}));
    let structured = &described["result"]["structuredContent"];
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

/// Closes the cargo-mutants gap on `test_describer_result`'s
/// `Ok(Default::default())`/`delete !` survivors and the
/// `MajServer::test_describer` wrapper survivor. Pins the actual confirmed
/// semantics against an unreachable backend: `describer_config::test`
/// propagates the probe's connection error through `?`, so `confirm_gate`
/// renders it exactly like a read tool's error — plain `isError: true` text
/// naming the URL, never a structured probe payload (matches the CLI's own
/// `describer_test_against_unreachable_backend_fails_with_context`).
#[test]
fn test_describer_dry_run_then_confirm_against_an_unreachable_backend_is_iserror() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    common::maj(&root, &state)
        .args([
            "describer",
            "set",
            "--backend",
            "ollama",
            "--model",
            "m",
            "--base-url",
            "http://127.0.0.1:1",
        ])
        .assert()
        .success();

    let mut mcp = Mcp::spawn(&root, &state);
    let dry = mcp.call_tool("test_describer", &serde_json::json!({}));
    assert_ne!(dry["result"]["isError"], serde_json::json!(true), "{dry}");
    let structured = &dry["result"]["structuredContent"];
    assert_eq!(
        structured["configured"]["model"],
        serde_json::json!("m"),
        "{structured}"
    );
    assert_eq!(
        structured["executed"],
        serde_json::json!(false),
        "{structured}"
    );

    let confirmed = mcp.call_tool("test_describer", &serde_json::json!({"confirm": true}));
    assert_eq!(
        confirmed["result"]["isError"],
        serde_json::json!(true),
        "an unreachable backend must report isError, not a silent success: {confirmed}"
    );
    let text = confirmed["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(text.contains("127.0.0.1:1"), "{text}");
}

/// Closes the cargo-mutants gap on `inbox_dry_run`'s `Ok(Default::default())`
/// survivor and the `MajServer::inbox_process` wrapper's `Ok(Default::
/// default())`/`delete !` survivors and one of its two match-guard variants
/// (the successful pass proves `failed` is correctly `false` here; the
/// `true`->`false` variant needs a failing pass, not covered by this test —
/// left open, same residual `sync_push_partial_failure_keeps_rows_and_maps_
/// polarity` already covers for its own sibling guard on the push side).
/// Fixture shape copied from `inbox_smoke.rs`'s `write_contribution`.
#[test]
fn inbox_process_dry_run_then_confirm_places_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("cat");
    let state = dir.path().join("state");
    common::maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    common::maj(&root, &state)
        .args(["para", "add", "project", "spring"])
        .assert()
        .success();
    let inbox = dir.path().join("inbox");
    let dest = dir.path().join("dest");
    std::fs::create_dir_all(&inbox).expect("mkdir");
    std::fs::create_dir_all(&dest).expect("mkdir");

    let drop = inbox.join("drop-1");
    std::fs::create_dir_all(&drop).expect("mkdir");
    let payload = b"mov-bytes-for-clip";
    std::fs::write(drop.join("clip.mov"), payload).expect("write");
    let hash = format!("{:016x}", xxhash_rust::xxh64::xxh64(payload, 0));
    let manifest = format!(
        r#"{{"version":1,"contributor":"dana","para_target":"project/spring","source":"iphone","files":[{{"name":"clip.mov","xxh64":"{hash}","size":{}}}]}}"#,
        payload.len()
    );
    std::fs::write(drop.join("contribution.json"), manifest).expect("write manifest");

    let mut mcp = Mcp::spawn(&root, &state);
    let dry = mcp.call_tool(
        "inbox_process",
        &serde_json::json!({
            "inbox": inbox.to_str().expect("utf8"),
            "dest": [dest.to_str().expect("utf8")],
        }),
    );
    assert_ne!(dry["result"]["isError"], serde_json::json!(true), "{dry}");
    let structured = &dry["result"]["structuredContent"];
    assert_eq!(
        structured["executed"],
        serde_json::json!(false),
        "{structured}"
    );
    let entries = structured["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(
        entries[0]["name"],
        serde_json::json!("drop-1"),
        "{entries:?}"
    );
    assert!(drop.is_dir(), "a dry run must not process anything");

    let confirmed = mcp.call_tool(
        "inbox_process",
        &serde_json::json!({
            "inbox": inbox.to_str().expect("utf8"),
            "dest": [dest.to_str().expect("utf8")],
            "confirm": true,
        }),
    );
    assert_ne!(
        confirmed["result"]["isError"],
        serde_json::json!(true),
        "{confirmed}"
    );
    let structured = &confirmed["result"]["structuredContent"];
    assert_eq!(
        structured["executed"],
        serde_json::json!(true),
        "{structured}"
    );
    let rows = structured["rows"].as_array().expect("rows array");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["name"], serde_json::json!("drop-1"), "{rows:?}");
    assert_eq!(
        rows[0]["outcome"]["Ingested"]["placed"],
        serde_json::json!(1),
        "{rows:?}"
    );

    assert!(
        dest.join("ascmhl").is_dir(),
        "verified ingest must write an ASC MHL history at dest"
    );
    assert!(
        inbox.join(".processed/drop-1/clip.mov").is_file(),
        "a successful pass moves the contribution to .processed/"
    );
    assert!(!drop.exists());
}

/// Closes the cargo-mutants gap on `sync_transfer_dry_run`'s
/// `Ok(Default::default())` survivor (plus its `==`->`!=` location filter,
/// exercised here via an explicit `location` argument) and the
/// `MajServer::sync_pull` wrapper's `Ok(Default::default())`/`delete !`
/// survivors and one of its two match-guard variants (same residual as
/// `inbox_process`'s test above — a failing pull isn't exercised here).
#[test]
fn sync_pull_dry_run_then_confirm_lands_a_pulled_asset() {
    let dir = tempfile::tempdir().expect("tempdir");
    let nas = dir.path().join("nas");
    std::fs::create_dir_all(&nas).expect("mkdir");

    // Machine 1: seeds one asset, pushes it to the shared location.
    let cat1 = dir.path().join("cat1");
    let state1 = dir.path().join("state1");
    common::maj_as(&cat1, &state1, "m1")
        .args(["catalog", "init"])
        .assert()
        .success();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(src.join("pulled.txt"), b"pulled-bytes").expect("write");
    common::maj_as(&cat1, &state1, "m1")
        .args(["scan", src.to_str().expect("utf8"), "--volume", "vol1"])
        .assert()
        .success();
    common::maj_as(&cat1, &state1, "m1")
        .args(["sync", "location", "add", "nas"])
        .arg(&nas)
        .assert()
        .success();
    common::maj_as(&cat1, &state1, "m1")
        .args(["sync", "push"])
        .assert()
        .success();

    // Machine 2: a separate catalog configured with the same location — the
    // one the MCP server serves.
    let cat2 = dir.path().join("cat2");
    let state2 = dir.path().join("state2");
    common::maj_as(&cat2, &state2, "m2")
        .args(["catalog", "init"])
        .assert()
        .success();
    common::maj_as(&cat2, &state2, "m2")
        .args(["sync", "location", "add", "nas"])
        .arg(&nas)
        .assert()
        .success();

    let mut mcp = Mcp::spawn(&cat2, &state2);
    let dry = mcp.call_tool("sync_pull", &serde_json::json!({"location": "nas"}));
    assert_ne!(dry["result"]["isError"], serde_json::json!(true), "{dry}");
    let structured = &dry["result"]["structuredContent"];
    assert_eq!(
        structured["executed"],
        serde_json::json!(false),
        "{structured}"
    );
    let planned = structured["planned"].as_array().expect("planned array");
    assert_eq!(planned.len(), 1, "{planned:?}");
    assert_eq!(planned[0]["name"], serde_json::json!("nas"), "{planned:?}");
    assert_eq!(
        planned[0]["reachable"],
        serde_json::json!(true),
        "{planned:?}"
    );
    assert!(
        !planned[0]["planned"]["segments"]
            .as_object()
            .expect("segments map")
            .is_empty(),
        "a dry-run pull must report the real behind segment count: {planned:?}"
    );

    let confirmed = mcp.call_tool("sync_pull", &serde_json::json!({"confirm": true}));
    assert_ne!(
        confirmed["result"]["isError"],
        serde_json::json!(true),
        "{confirmed}"
    );
    let structured = &confirmed["result"]["structuredContent"];
    assert_eq!(
        structured["executed"],
        serde_json::json!(true),
        "{structured}"
    );
    assert!(
        structured["applied_events"]
            .as_u64()
            .expect("applied_events")
            >= 1,
        "{structured}"
    );

    // Cross-process verification: the pulled asset is searchable through
    // this same MCP server, re-opening machine 2's catalog fresh.
    let found = mcp.call_tool("search_assets", &serde_json::json!({"query": "pulled.txt"}));
    assert_ne!(
        found["result"]["isError"],
        serde_json::json!(true),
        "{found}"
    );
    assert_eq!(
        found["result"]["structuredContent"]["count"],
        serde_json::json!(1),
        "{found}"
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
