mod common;
use common::maj;
use predicates::prelude::*;
use predicates::str::contains;

#[test]
fn describer_set_show_round_trip_redacts_key() {
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
            "qwen/qwen3-vl-8b",
            "--api-key",
            "sk-secret",
        ])
        .assert()
        .success()
        .stdout(contains("open-router").and(contains("qwen/qwen3-vl-8b")));

    maj(&root, &state)
        .args(["describer", "show"])
        .assert()
        .success()
        .stdout(contains("open-router"))
        .stdout(contains("(redacted)"))
        .stdout(contains("sk-secret").not());
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
