//! Integration tests exercising the public library surface end-to-end.

use argus::search::SearchEngine;
use argus::types::{IndexConfig, SearchConfig};
use std::fs;
use tempfile::tempdir;

fn config_for(dir: &std::path::Path, pattern: &str) -> SearchConfig {
    SearchConfig {
        directory: dir.to_path_buf(),
        pattern: pattern.into(),
        ..Default::default()
    }
}

#[test]
fn finds_match_in_nested_directory() {
    let dir = tempdir().unwrap();
    let nested = dir.path().join("a").join("b");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("note.txt"), "deeply buried needle").unwrap();

    let mut engine =
        SearchEngine::new(config_for(dir.path(), "needle"), IndexConfig::default()).unwrap();
    let (results, stats) = engine.search();
    assert_eq!(results.len(), 1);
    assert_eq!(stats.total_matches, 1);
    assert!(results[0].path.to_string_lossy().ends_with("note.txt"));
}

#[test]
fn multiple_file_types_aggregated() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("readme.md"), "TODO: refactor").unwrap();
    fs::write(dir.path().join("main.rs"), "// TODO: fix this").unwrap();
    fs::write(dir.path().join("notes.txt"), "no match here").unwrap();

    let mut engine =
        SearchEngine::new(config_for(dir.path(), "TODO"), IndexConfig::default()).unwrap();
    let (results, stats) = engine.search();
    assert_eq!(results.len(), 2);
    assert_eq!(stats.files_scanned, 3);
    assert_eq!(stats.files_matched, 2);
}

#[test]
fn regex_with_case_sensitivity() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "Error: 42\nerror: 7\nOKAY").unwrap();

    let config = SearchConfig {
        directory: dir.path().to_path_buf(),
        pattern: r"^Error:\s+\d+".into(),
        use_regex: true,
        case_sensitive: true,
        ..Default::default()
    };
    let mut engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
    let (results, _) = engine.search();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].matches.len(), 1);
    assert_eq!(results[0].matches[0].matched_text, "Error: 42");
}

#[test]
fn binary_files_are_skipped() {
    let dir = tempdir().unwrap();
    // A file full of null bytes with text-like extension should still be
    // classified as binary and skipped.
    fs::write(dir.path().join("blob.dat"), vec![0u8; 4096]).unwrap();
    fs::write(dir.path().join("real.txt"), "hello match").unwrap();

    let mut engine =
        SearchEngine::new(config_for(dir.path(), "match"), IndexConfig::default()).unwrap();
    let (results, _) = engine.search();
    assert_eq!(results.len(), 1);
    assert!(results[0].path.to_string_lossy().ends_with("real.txt"));
}

#[test]
fn index_reused_after_initial_save() {
    let search_dir = tempdir().unwrap();
    let index_dir = tempdir().unwrap();
    fs::write(search_dir.path().join("a.txt"), "alpha beta gamma").unwrap();
    fs::write(search_dir.path().join("b.txt"), "only gamma here").unwrap();
    let index_path = index_dir.path().join("index.json");

    // First pass: save index.
    let config = config_for(search_dir.path(), "gamma");
    let save_cfg = IndexConfig {
        save_index: true,
        use_index: false,
        index_file: Some(index_path.clone()),
    };
    let mut engine = SearchEngine::new(config.clone(), save_cfg).unwrap();
    let (results_first, _) = engine.search();
    assert_eq!(results_first.len(), 2);
    assert!(index_path.exists());

    // Second pass: reuse the cached text from the index.
    let use_cfg = IndexConfig {
        save_index: false,
        use_index: true,
        index_file: Some(index_path.clone()),
    };
    let mut engine = SearchEngine::new(config, use_cfg).unwrap();
    let (results_second, _) = engine.search();
    assert_eq!(results_second.len(), 2);
}

#[test]
fn index_survives_file_modification() {
    let search_dir = tempdir().unwrap();
    let index_dir = tempdir().unwrap();
    let file = search_dir.path().join("doc.txt");
    fs::write(&file, "alpha alpha").unwrap();
    let index_path = index_dir.path().join("idx.json");

    let config = config_for(search_dir.path(), "alpha");
    let save_cfg = IndexConfig {
        save_index: true,
        use_index: false,
        index_file: Some(index_path.clone()),
    };
    let mut engine = SearchEngine::new(config.clone(), save_cfg).unwrap();
    let (_, stats) = engine.search();
    assert_eq!(stats.total_matches, 2);

    // Modify the file so the cached entry is invalid.
    // Sleep briefly to ensure the mtime/size differ.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(&file, "alpha alpha alpha alpha").unwrap();

    let use_cfg = IndexConfig {
        save_index: true,
        use_index: true,
        index_file: Some(index_path.clone()),
    };
    let mut engine = SearchEngine::new(config, use_cfg).unwrap();
    let (_, stats) = engine.search();
    assert_eq!(stats.total_matches, 4);
}

#[test]
fn extensions_filter_cooperates_with_skipped_dirs() {
    let dir = tempdir().unwrap();
    let target_dir = dir.path().join("target");
    fs::create_dir_all(&target_dir).unwrap();
    fs::write(target_dir.join("binary.rs"), "needle").unwrap();
    fs::write(dir.path().join("lib.rs"), "needle").unwrap();

    let config = SearchConfig {
        directory: dir.path().to_path_buf(),
        pattern: "needle".into(),
        extensions: vec!["rs".into()],
        ..Default::default()
    };
    let mut engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
    let (results, _) = engine.search();
    assert_eq!(results.len(), 1);
    assert!(results[0].path.to_string_lossy().ends_with("lib.rs"));
}
