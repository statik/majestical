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
