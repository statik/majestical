mod common;
use common::maj;
use predicates::prelude::*;
use predicates::str::contains;

/// A fresh catalog under `tmp`, returned as `(root, state)`.
fn init_catalog(tmp: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = tmp.join("cat");
    let state = tmp.join("state");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    (root, state)
}

fn set_openrouter(root: &std::path::Path, state: &std::path::Path, model: &str, key: Option<&str>) {
    let mut cmd = maj(root, state);
    cmd.env_remove("MAJ_OPENROUTER_KEY")
        .args(["describer", "set", "--backend", "open-router", "--model"])
        .arg(model);
    if let Some(key) = key {
        cmd.args(["--api-key", key]);
    }
    let echo = cmd
        .assert()
        .success()
        .stdout(contains("open-router").and(contains(model)));
    if let Some(key) = key {
        echo.stdout(contains(key).not());
    }
}

#[test]
fn describer_set_show_round_trip_names_the_file_and_never_the_key() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, state) = init_catalog(tmp.path());
    set_openrouter(&root, &state, "qwen/qwen3-vl-8b", Some("sk-test"));

    maj(&root, &state)
        .env_remove("MAJ_OPENROUTER_KEY")
        .args(["describer", "show"])
        .assert()
        .success()
        .stdout(contains("open-router"))
        .stdout(contains("api-key:  (from file)"))
        .stdout(contains("sk-test").not());
}

/// `set` without `--api-key` changes the model and leaves the stored key
/// where it was, rather than silently dropping it.
#[test]
fn describer_set_without_a_key_keeps_the_stored_one() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, state) = init_catalog(tmp.path());
    set_openrouter(&root, &state, "first-model", Some("sk-test"));
    set_openrouter(&root, &state, "second-model", None);

    maj(&root, &state)
        .env_remove("MAJ_OPENROUTER_KEY")
        .args(["describer", "show"])
        .assert()
        .success()
        .stdout(contains("second-model"))
        .stdout(contains("api-key:  (from file)"));
}

#[test]
fn describer_show_names_the_env_as_the_keys_source() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, state) = init_catalog(tmp.path());
    set_openrouter(&root, &state, "some-model", None);

    maj(&root, &state)
        .env_remove("MAJ_OPENROUTER_KEY")
        .args(["describer", "show"])
        .assert()
        .success()
        .stdout(contains("api-key:  (none)"));
    maj(&root, &state)
        .env("MAJ_OPENROUTER_KEY", "sk-test")
        .args(["describer", "show"])
        .assert()
        .success()
        .stdout(contains("api-key:  (from env)"))
        .stdout(contains("sk-test").not());
}

#[test]
fn describer_show_without_config_names_the_remedy() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["describer", "show"])
        .assert()
        .success()
        .stdout(contains("no describer configured").and(contains("maj describer set")));
}

#[test]
fn describer_set_defaults_base_url_per_backend() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args([
            "describer",
            "set",
            "--backend",
            "ollama",
            "--model",
            "qwen3-vl:8b",
        ])
        .assert()
        .success()
        .stdout(contains("http://localhost:11434"));
}

#[test]
fn describer_test_against_unreachable_backend_fails_with_context() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
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
    maj(&root, &state)
        .args(["describer", "test"])
        .assert()
        .failure()
        .stderr(contains("127.0.0.1:1"));
}

/// An `OpenRouter` catalog pointed at a mock that lists model `m` and
/// answers the key endpoint with `key_status`, then `maj describer test`
/// run with the key in the environment only — never on a command line or
/// in the file. Returns the child's stdout and stderr.
#[cfg(test)]
fn describer_test_with_the_key_answered(key_status: u16) -> (String, String) {
    use httpmock::prelude::{GET, MockServer};

    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/v1/models");
        then.status(200)
            .json_body(serde_json::json!({"data": [{"id": "m"}]}));
    });
    let key = server.mock(|when, then| {
        when.method(GET).path("/v1/key");
        then.status(key_status).json_body(serde_json::json!({}));
    });

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args([
            "describer",
            "set",
            "--backend",
            "open-router",
            "--model",
            "m",
            "--base-url",
            &server.base_url(),
        ])
        .assert()
        .success();

    let out = maj(&root, &state)
        .env("MAJ_OPENROUTER_KEY", "sk-test")
        .args(["describer", "test"])
        .output()
        .expect("run maj describer test");
    assert!(out.status.success(), "{out:?}");
    key.assert_calls(1);
    (
        String::from_utf8(out.stdout).expect("utf-8 stdout"),
        String::from_utf8(out.stderr).expect("utf-8 stderr"),
    )
}

/// A rejected key is reported by name, and the line promising caption work
/// must not follow it — the two together would contradict each other. The
/// key itself appears nowhere in the output.
#[test]
fn describer_test_reports_a_rejected_key_and_does_not_promise_captions() {
    let (stdout, stderr) = describer_test_with_the_key_answered(401);

    assert!(
        stdout
            .lines()
            .any(|line| line == "key: REJECTED — OpenRouter answered 401; set a new key"),
        "{stdout}"
    );
    assert!(!stdout.contains("will run on the next"), "{stdout}");
    assert!(!stdout.contains("sk-test"), "{stdout}");
    assert!(!stderr.contains("sk-test"), "{stderr}");
}

#[test]
fn describer_test_reports_an_accepted_key() {
    let (stdout, stderr) = describer_test_with_the_key_answered(200);

    assert!(
        stdout.lines().any(|line| line == "key: accepted"),
        "{stdout}"
    );
    assert!(stdout.contains("will run on the next"), "{stdout}");
    assert!(!stdout.contains("sk-test"), "{stdout}");
    assert!(!stderr.contains("sk-test"), "{stderr}");
}

/// `OpenRouter` with no key anywhere: the next caption pass would fail every
/// item for want of one, so `describer test` says so and withholds the
/// promise — without asking the key endpoint about a key it does not have.
#[test]
fn describer_test_without_a_key_says_so_and_does_not_promise_captions() {
    use httpmock::prelude::{GET, MockServer};

    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/v1/models");
        then.status(200)
            .json_body(serde_json::json!({"data": [{"id": "m"}]}));
    });
    let key = server.mock(|when, then| {
        when.method(GET).path("/v1/key");
        then.status(401).json_body(serde_json::json!({}));
    });

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args([
            "describer",
            "set",
            "--backend",
            "open-router",
            "--model",
            "m",
            "--base-url",
            &server.base_url(),
        ])
        .assert()
        .success();

    let out = maj(&root, &state)
        .env_remove("MAJ_OPENROUTER_KEY")
        .args(["describer", "test"])
        .output()
        .expect("run maj describer test");
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");

    assert!(
        stdout.lines().any(|line| line
            == "key: MISSING — save one in Settings → Captions, or set it with \
                `maj describer set --api-key` or MAJ_OPENROUTER_KEY"),
        "{stdout}"
    );
    assert!(!stdout.contains("will run on the next"), "{stdout}");
    key.assert_calls(0);
}

/// A catalog whose `describer.toml` is broken on the line that holds a key
/// — the line a TOML parse error would quote back. It is line 4.
#[cfg(test)]
fn catalog_with_a_broken_config(tmp: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let (root, state) = init_catalog(tmp);
    common::break_describer_config(
        &root,
        &state,
        &format!(
            "{}api_key = \"sk-test\" oops\n",
            common::DESCRIBER_CONFIG_HEAD
        ),
    );
    (root, state)
}

/// Runs `maj` with `args` over the broken config and returns
/// `(succeeded, stdout, stderr)`, having checked that neither stream
/// carries the key.
#[cfg(test)]
fn run_over_a_broken_config(args: &[&str]) -> (bool, String, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let model_dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = catalog_with_a_broken_config(tmp.path());
    let out = maj(&root, &state)
        .env_remove("MAJ_OPENROUTER_KEY")
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(args)
        .output()
        .expect("run maj");
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(!stdout.contains("sk-test"), "{args:?} stdout: {stdout}");
    assert!(!stderr.contains("sk-test"), "{args:?} stderr: {stderr}");
    (out.status.success(), stdout, stderr)
}

#[test]
fn describer_show_on_a_broken_config_names_the_line_and_never_quotes_it() {
    let (succeeded, _, stderr) = run_over_a_broken_config(&["describer", "show"]);
    assert!(!succeeded);
    assert!(stderr.contains("describer.toml: line 4: "), "{stderr}");
}

#[test]
fn describer_test_on_a_broken_config_names_the_line_and_never_quotes_it() {
    let (succeeded, _, stderr) = run_over_a_broken_config(&["describer", "test"]);
    assert!(!succeeded);
    assert!(stderr.contains("describer.toml: line 4: "), "{stderr}");
}

/// `set` without `--api-key` would have to carry the file's key forward, so
/// it refuses a file it can't parse rather than dropping that key.
#[test]
fn describer_set_on_a_broken_config_names_the_line_and_never_quotes_it() {
    let (succeeded, _, stderr) =
        run_over_a_broken_config(&["describer", "set", "--backend", "ollama", "--model", "m2"]);
    assert!(!succeeded);
    assert!(stderr.contains("describer.toml: line 4: "), "{stderr}");
}

/// A broken describer config degrades captions rather than failing `index
/// status`, which reports it as a notice: on stderr, and in `--json` among
/// the outcome's `notices`.
#[test]
fn index_status_on_a_broken_config_names_the_line_and_never_quotes_it() {
    let (succeeded, _, stderr) = run_over_a_broken_config(&["index", "status"]);
    assert!(succeeded, "{stderr}");
    assert!(stderr.contains("describer.toml: line 4: "), "{stderr}");

    let (succeeded, stdout, stderr) = run_over_a_broken_config(&["index", "status", "--json"]);
    assert!(succeeded, "{stderr}");
    assert!(
        format!("{stdout}{stderr}").contains("describer.toml: line 4: "),
        "{stdout}{stderr}"
    );
}

/// A key pasted into the wrong field: `backend` is the one field that
/// refuses a string, and its refusal lists the set, not the value.
#[test]
fn describer_show_with_a_key_pasted_into_backend_never_quotes_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, state) = init_catalog(tmp.path());
    common::break_describer_config(
        &root,
        &state,
        "model = \"m\"\nbackend = \"sk-test\"\nbase_url = \"u\"\n",
    );

    let out = maj(&root, &state)
        .env_remove("MAJ_OPENROUTER_KEY")
        .args(["describer", "show"])
        .output()
        .expect("run maj describer show");
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");

    assert!(!out.status.success());
    assert!(
        stderr.contains(
            "describer.toml: line 2: unknown backend — expected one of ollama, lm-studio, \
             open-router"
        ),
        "{stderr}"
    );
    assert!(!stdout.contains("sk-test"), "{stdout}");
    assert!(!stderr.contains("sk-test"), "{stderr}");
}
