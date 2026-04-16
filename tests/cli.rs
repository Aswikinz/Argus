//! End-to-end tests that exercise the `argus` binary via the command line.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

fn argus() -> Command {
    Command::cargo_bin("argus").expect("binary should build")
}

#[test]
fn prints_help() {
    argus()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Argus"))
        .stdout(predicate::str::contains("--directory"));
}

#[test]
fn reports_version() {
    argus()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn fails_without_pattern() {
    argus()
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn reports_missing_directory() {
    argus()
        .args([
            "--no-banner",
            "--non-interactive",
            "-d",
            "/definitely/not/a/real/path/xyzzy",
            "needle",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not exist"));
}

#[test]
fn finds_matches_non_interactive() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "find the needle here").unwrap();

    argus()
        .args([
            "--no-banner",
            "--non-interactive",
            "-d",
            dir.path().to_str().unwrap(),
            "needle",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("a.txt"))
        .stdout(predicate::str::contains("1 matches").or(predicate::str::contains("matches")));
}

#[test]
fn no_matches_prints_message() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "nothing interesting").unwrap();

    argus()
        .args([
            "--no-banner",
            "--non-interactive",
            "-d",
            dir.path().to_str().unwrap(),
            "zzzzzzzz_no_hit",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("no matches"));
}

#[test]
fn invalid_regex_exits_with_error() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "anything").unwrap();

    argus()
        .args([
            "--no-banner",
            "--non-interactive",
            "-r",
            "-d",
            dir.path().to_str().unwrap(),
            "[",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Invalid regex"));
}

#[test]
fn extension_filter_narrows_results() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "needle").unwrap();
    fs::write(dir.path().join("b.log"), "needle").unwrap();

    let assert = argus()
        .args([
            "--no-banner",
            "--non-interactive",
            "-d",
            dir.path().to_str().unwrap(),
            "-e",
            "txt",
            "needle",
        ])
        .assert()
        .success();
    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(output.contains("a.txt"));
    assert!(!output.contains("b.log"));
}
