//! Search engine with parallel processing.

use crate::extractors::{extract_text, is_binary_file};
use crate::index::{get_file_timestamp, Index, IndexEntry};
use crate::types::{FileType, IndexConfig, Match, SearchConfig, SearchResult, SearchStats};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use regex::{Regex, RegexBuilder};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use walkdir::{DirEntry, WalkDir};

/// Shared progress counters that external callers (e.g. the TUI) can poll to
/// render a live progress bar while the search runs on a worker thread.
///
/// `total` is set to the number of discovered files once `collect_files`
/// completes — it is `0` until then. `current` monotonically increases as
/// files finish processing.
#[derive(Debug, Default)]
pub struct ProgressHandle {
    pub current: AtomicUsize,
    pub total: AtomicUsize,
}

/// The search engine that coordinates file discovery and text matching.
pub struct SearchEngine {
    config: SearchConfig,
    index_config: IndexConfig,
    pattern: SearchPattern,
    index: Option<Index>,
    progress: Arc<ProgressHandle>,
    quiet: bool,
}

/// Compiled search pattern (either regex or literal).
enum SearchPattern {
    Regex(Regex),
    Literal { pattern: String, lowercase: String },
}

impl SearchEngine {
    /// Create a new search engine with the given configuration.
    pub fn new(config: SearchConfig, index_config: IndexConfig) -> Result<Self, regex::Error> {
        let pattern = if config.use_regex {
            let regex = RegexBuilder::new(&config.pattern)
                .case_insensitive(!config.case_sensitive)
                .multi_line(true)
                .build()?;
            SearchPattern::Regex(regex)
        } else {
            SearchPattern::Literal {
                pattern: config.pattern.clone(),
                lowercase: config.pattern.to_lowercase(),
            }
        };

        // Try to load existing index if use_index is enabled. The load-status
        // eprintln is intentionally omitted: CLI callers surface the info via
        // a distinct human-facing line, and TUI callers can't take any extra
        // output on stderr without corrupting ratatui's screen.
        let index = if index_config.use_index || index_config.save_index {
            let index_path = index_config.get_index_path(&config.directory);
            match Index::load(&index_path) {
                Ok(idx) => Some(idx),
                Err(_) => {
                    if index_config.save_index {
                        // Create new index if we're going to save
                        Some(Index::new(config.directory.clone()))
                    } else {
                        None
                    }
                }
            }
        } else {
            None
        };

        Ok(Self {
            config,
            index_config,
            pattern,
            index,
            progress: Arc::new(ProgressHandle::default()),
            quiet: false,
        })
    }

    /// Clone of the shared progress counters. Safe to retain across a
    /// [`Self::search`] call on another thread — the values keep updating
    /// while the search runs and freeze when it finishes.
    pub fn progress_handle(&self) -> Arc<ProgressHandle> {
        self.progress.clone()
    }

    /// Suppress the built-in `indicatif` progress bar. Callers that render
    /// their own progress (the TUI) must enable this, otherwise the two
    /// renderers fight for stdout/stderr and the screen goes haywire.
    pub fn set_quiet(&mut self, quiet: bool) {
        self.quiet = quiet;
    }

    /// Execute the search and return results.
    pub fn search(&mut self) -> (Vec<SearchResult>, SearchStats) {
        let start = Instant::now();

        // Reset progress counters so repeated `search` calls on the same
        // engine start from zero instead of continuing a previous run.
        self.progress.current.store(0, Ordering::Relaxed);
        self.progress.total.store(0, Ordering::Relaxed);

        // Collect all files to search
        let files = self.collect_files();
        let total_files = files.len();
        self.progress.total.store(total_files, Ordering::Relaxed);

        // Built-in indicatif bar for CLI callers. When `quiet` is set (the
        // TUI path) we use a hidden bar so no escape codes reach stdout and
        // fight with ratatui's rendering.
        let pb = if self.quiet {
            ProgressBar::hidden()
        } else {
            let pb = ProgressBar::new(total_files as u64);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("  {msg}  {pos}/{len}  {wide_bar}  {percent}%")
                    .unwrap()
                    .progress_chars("━━─"),
            );
            pb.set_message("searching");
            pb
        };

        // Thread-safe containers for results and stats
        let results: Arc<Mutex<Vec<SearchResult>>> = Arc::new(Mutex::new(Vec::new()));
        let stats = Arc::new(Mutex::new(SearchStats::new()));
        let new_index_entries: Arc<Mutex<Vec<IndexEntry>>> = Arc::new(Mutex::new(Vec::new()));

        // Clone index for thread-safe access
        let index_ref = self.index.as_ref().map(|i| Arc::new(i.clone()));
        let save_index = self.index_config.save_index;
        let progress = self.progress.clone();

        // Process files in parallel using rayon
        files.par_iter().for_each(|file_path| {
            let result = self.search_file_with_index(
                file_path,
                index_ref.as_ref(),
                &new_index_entries,
                save_index,
            );

            // Update stats
            {
                let mut stats_guard = stats.lock().unwrap();
                stats_guard.inc_scanned();

                if let Some(ref res) = result {
                    if res.error.is_some() {
                        stats_guard.inc_skipped();
                    } else {
                        stats_guard.add_result(res);
                    }
                }
            }

            // Store result if it has matches
            if let Some(res) = result {
                if !res.matches.is_empty() {
                    let mut results_guard = results.lock().unwrap();
                    results_guard.push(res);
                }
            }

            // Update progress
            let processed = progress.current.fetch_add(1, Ordering::Relaxed) + 1;
            pb.set_position(processed as u64);
        });

        pb.finish_and_clear();

        // Update index with new entries if save_index is enabled
        if self.index_config.save_index {
            if let Some(ref mut index) = self.index {
                let entries = Arc::try_unwrap(new_index_entries).map_or_else(
                    |arc| arc.lock().unwrap().clone(),
                    |mutex| mutex.into_inner().unwrap(),
                );

                for entry in entries {
                    index.upsert_entry(entry);
                }

                // Prune entries for files that no longer exist
                index.prune_missing();

                // Save the index
                let index_path = self.index_config.get_index_path(&self.config.directory);
                if let Err(e) = index.save(&index_path) {
                    if !self.quiet {
                        eprintln!("  warning: failed to save index: {e}");
                    }
                } else if !self.quiet {
                    eprintln!(
                        "  saved index with {} entries to {}",
                        index.len(),
                        index_path.display()
                    );
                }
            }
        }

        // Get final results and stats
        let mut final_results = Arc::try_unwrap(results).map_or_else(
            |arc| arc.lock().unwrap().clone(),
            |mutex| mutex.into_inner().unwrap(),
        );
        let mut final_stats = Arc::try_unwrap(stats).map_or_else(
            |arc| arc.lock().unwrap().clone(),
            |mutex| mutex.into_inner().unwrap(),
        );

        // Sort results by match count (descending)
        final_results.sort();

        // Limit results
        if final_results.len() > self.config.limit {
            final_results.truncate(self.config.limit);
        }

        // Record duration
        final_stats.duration_ms = start.elapsed().as_millis() as u64;

        (final_results, final_stats)
    }

    /// Collect all files to search based on configuration.
    fn collect_files(&self) -> Vec<PathBuf> {
        let mut walker = WalkDir::new(&self.config.directory);

        // Set max depth if specified
        if let Some(depth) = self.config.max_depth {
            walker = walker.max_depth(depth);
        }

        // Convert extensions to a set for fast lookup
        let extensions: HashSet<String> = self
            .config
            .extensions
            .iter()
            .map(|e| e.to_lowercase().trim_start_matches('.').to_string())
            .collect();

        // Resolve the index file path so we can always exclude it from the
        // walk. Without this filter, a cached index with previously-matched
        // text would itself become a "match" on the next search — confusing
        // and wrong. We compare both the raw and canonicalized forms so the
        // exclusion works whether or not the index lives inside the search
        // root or was passed as a relative path.
        let index_path_raw = self.index_config.get_index_path(&self.config.directory);
        let index_path_abs = index_path_raw.canonicalize().ok();

        walker
            .into_iter()
            .filter_entry(|e| self.should_process_entry(e))
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_type().is_file())
            .filter(|e| {
                // Always exclude the Argus index file (both the configured one
                // and any bare `.argus_index.json` that happens to be lying
                // around). This rule runs even when --hidden is set.
                let path = e.path();
                if path.file_name().and_then(|n| n.to_str()) == Some(".argus_index.json") {
                    return false;
                }
                if path == index_path_raw {
                    return false;
                }
                if let Some(ref abs) = index_path_abs {
                    if path.canonicalize().ok().as_ref() == Some(abs) {
                        return false;
                    }
                }
                true
            })
            .filter(|e| {
                // Filter by extension if specified
                if extensions.is_empty() {
                    true
                } else {
                    e.path().extension().is_some_and(|ext| {
                        extensions.contains(&ext.to_string_lossy().to_lowercase())
                    })
                }
            })
            .filter(|e| {
                // Skip binary files (except PDFs and images which we handle specially)
                let ext = e
                    .path()
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                let file_type = FileType::from_extension(&ext);

                match file_type {
                    FileType::Pdf | FileType::Docx => true,
                    FileType::Image => self.config.ocr.enabled,
                    _ => !is_binary_file(e.path()),
                }
            })
            .map(|e| e.path().to_path_buf())
            .collect()
    }

    /// Check if a directory entry should be processed.
    fn should_process_entry(&self, entry: &DirEntry) -> bool {
        // Always process the root directory
        if entry.depth() == 0 {
            return true;
        }

        let name = entry.file_name().to_string_lossy();

        // Skip hidden files/directories unless configured to include them
        if !self.config.include_hidden && name.starts_with('.') {
            return false;
        }

        // Skip common non-essential directories
        let skip_dirs = [
            "node_modules",
            "target",
            "__pycache__",
            ".git",
            ".svn",
            ".hg",
            "vendor",
            "dist",
            "build",
            ".cache",
            ".npm",
            ".cargo",
        ];

        if entry.file_type().is_dir() && skip_dirs.contains(&name.as_ref()) {
            return false;
        }

        true
    }

    /// Search a single file for matches, using the index when available.
    fn search_file_with_index(
        &self,
        path: &PathBuf,
        index: Option<&Arc<Index>>,
        new_entries: &Arc<Mutex<Vec<IndexEntry>>>,
        save_index: bool,
    ) -> Option<SearchResult> {
        // Determine file type
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();
        let file_type = FileType::from_extension(&ext);

        // Get file metadata
        let metadata = path.metadata().ok()?;
        let file_size = metadata.len();
        let modified_timestamp = get_file_timestamp(path).unwrap_or(0);

        // Try to get text from index first
        let text = if let Some(idx) = index {
            if let Some(entry) = idx.get_valid_entry(path) {
                // Use cached text
                entry.extracted_text.clone()
            } else {
                // Extract text and optionally add to index
                let extraction = extract_text(path, file_type, &self.config.ocr);

                if !extraction.success {
                    return Some(SearchResult::with_error(
                        path.clone(),
                        file_type,
                        extraction
                            .error
                            .unwrap_or_else(|| "Unknown error".to_string()),
                    ));
                }

                // Queue new entry for index if save_index is enabled
                if save_index {
                    let entry = IndexEntry::new(
                        path.clone(),
                        file_type,
                        extraction.text.clone(),
                        modified_timestamp,
                        file_size,
                    );
                    new_entries.lock().unwrap().push(entry);
                }

                extraction.text
            }
        } else {
            // No index - extract text normally
            let extraction = extract_text(path, file_type, &self.config.ocr);

            if !extraction.success {
                return Some(SearchResult::with_error(
                    path.clone(),
                    file_type,
                    extraction
                        .error
                        .unwrap_or_else(|| "Unknown error".to_string()),
                ));
            }

            extraction.text
        };

        // Search for matches
        let matches = self.find_matches(&text);

        if matches.is_empty() {
            None
        } else {
            Some(SearchResult::new(
                path.clone(),
                file_type,
                matches,
                file_size,
            ))
        }
    }

    /// Search a single file for matches (without index).
    #[allow(dead_code)]
    fn search_file(&self, path: &Path) -> Option<SearchResult> {
        // Determine file type
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();
        let file_type = FileType::from_extension(&ext);

        // Get file size
        let file_size = path.metadata().map_or(0, |m| m.len());

        // Extract text
        let extraction = extract_text(path, file_type, &self.config.ocr);

        if !extraction.success {
            return Some(SearchResult::with_error(
                path.to_path_buf(),
                file_type,
                extraction
                    .error
                    .unwrap_or_else(|| "Unknown error".to_string()),
            ));
        }

        // Search for matches
        let matches = self.find_matches(&extraction.text);

        if matches.is_empty() {
            None
        } else {
            Some(SearchResult::new(
                path.to_path_buf(),
                file_type,
                matches,
                file_size,
            ))
        }
    }

    /// Find all matches in the given text.
    fn find_matches(&self, text: &str) -> Vec<Match> {
        match &self.pattern {
            SearchPattern::Regex(regex) => self.find_regex_matches(text, regex),
            SearchPattern::Literal { pattern, lowercase } => {
                self.find_literal_matches(text, pattern, lowercase)
            }
        }
    }

    /// Find matches using regex.
    fn find_regex_matches(&self, text: &str, regex: &Regex) -> Vec<Match> {
        let mut matches = Vec::new();
        let lines: Vec<&str> = text.lines().collect();

        for line in lines.iter() {
            for mat in regex.find_iter(line) {
                matches.push(Match::new(mat.as_str().to_string(), line.to_string()));
            }
        }

        matches
    }

    /// Find matches using literal string search.
    fn find_literal_matches(&self, text: &str, pattern: &str, lowercase: &str) -> Vec<Match> {
        let mut matches = Vec::new();
        let lines: Vec<&str> = text.lines().collect();

        for line in lines.iter() {
            let search_line = if self.config.case_sensitive {
                line.to_string()
            } else {
                line.to_lowercase()
            };

            let search_pattern = if self.config.case_sensitive {
                pattern
            } else {
                lowercase
            };

            let mut start = 0;
            while let Some(pos) = search_line[start..].find(search_pattern) {
                let actual_pos = start + pos;
                let matched_text = &line[actual_pos..actual_pos + pattern.len()];

                matches.push(Match::new(matched_text.to_string(), line.to_string()));

                start = actual_pos + 1;
                if start >= search_line.len() {
                    break;
                }
            }
        }

        matches
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_literal_search() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "Hello World\nHello Rust\nGoodbye World").unwrap();

        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "Hello".to_string(),
            ..Default::default()
        };
        let index_config = IndexConfig::default();

        let mut engine = SearchEngine::new(config, index_config).unwrap();
        let (results, stats) = engine.search();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].matches.len(), 2);
        assert_eq!(stats.total_matches, 2);
    }

    #[test]
    fn test_case_insensitive_search() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "hello HELLO Hello").unwrap();

        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "hello".to_string(),
            case_sensitive: false,
            ..Default::default()
        };
        let index_config = IndexConfig::default();

        let mut engine = SearchEngine::new(config, index_config).unwrap();
        let (results, _) = engine.search();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].matches.len(), 3);
    }

    #[test]
    fn test_regex_search() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "foo123 bar456 baz789").unwrap();

        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: r"\w+\d+".to_string(),
            use_regex: true,
            ..Default::default()
        };
        let index_config = IndexConfig::default();

        let mut engine = SearchEngine::new(config, index_config).unwrap();
        let (results, _) = engine.search();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].matches.len(), 3);
    }

    #[test]
    fn test_invalid_regex_returns_error() {
        let config = SearchConfig {
            directory: PathBuf::from("."),
            pattern: "[".into(),
            use_regex: true,
            ..Default::default()
        };
        let result = SearchEngine::new(config, IndexConfig::default());
        assert!(result.is_err());
    }

    #[test]
    fn test_case_sensitive_literal_search() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "Hello HELLO hello").unwrap();
        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "Hello".into(),
            case_sensitive: true,
            ..Default::default()
        };
        let mut engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
        let (results, _) = engine.search();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].matches.len(), 1);
    }

    #[test]
    fn test_no_matches_returns_empty() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "nothing here").unwrap();
        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "zzzzz".into(),
            ..Default::default()
        };
        let mut engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
        let (results, stats) = engine.search();
        assert!(results.is_empty());
        assert_eq!(stats.files_matched, 0);
    }

    #[test]
    fn test_extension_filter() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "match").unwrap();
        fs::write(dir.path().join("b.log"), "match").unwrap();

        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "match".into(),
            extensions: vec!["txt".into()],
            ..Default::default()
        };
        let mut engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
        let (results, _) = engine.search();
        assert_eq!(results.len(), 1);
        assert!(results[0].path.to_string_lossy().ends_with("a.txt"));
    }

    #[test]
    fn test_extension_filter_with_dot_prefix() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "match").unwrap();
        fs::write(dir.path().join("b.log"), "match").unwrap();

        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "match".into(),
            extensions: vec![".txt".into()],
            ..Default::default()
        };
        let mut engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
        let (results, _) = engine.search();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_hidden_files_skipped_by_default() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".hidden.txt"), "secret match").unwrap();
        fs::write(dir.path().join("visible.txt"), "secret match").unwrap();

        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "secret".into(),
            ..Default::default()
        };
        let mut engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
        let (results, _) = engine.search();
        assert_eq!(results.len(), 1);
        assert!(results[0].path.to_string_lossy().ends_with("visible.txt"));
    }

    #[test]
    fn test_hidden_files_included_when_enabled() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".hidden.txt"), "secret match").unwrap();
        fs::write(dir.path().join("visible.txt"), "secret match").unwrap();

        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "secret".into(),
            include_hidden: true,
            ..Default::default()
        };
        let mut engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
        let (results, _) = engine.search();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_max_depth_limits_descent() {
        let dir = tempdir().unwrap();
        let deep = dir.path().join("a").join("b").join("c");
        fs::create_dir_all(&deep).unwrap();
        fs::write(dir.path().join("top.txt"), "needle").unwrap();
        fs::write(deep.join("deep.txt"), "needle").unwrap();

        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "needle".into(),
            max_depth: Some(1),
            ..Default::default()
        };
        let mut engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
        let (results, _) = engine.search();
        assert_eq!(results.len(), 1);
        assert!(results[0].path.to_string_lossy().ends_with("top.txt"));
    }

    #[test]
    fn test_skipped_dirs_ignored() {
        let dir = tempdir().unwrap();
        let nm = dir.path().join("node_modules");
        fs::create_dir_all(&nm).unwrap();
        fs::write(nm.join("bundle.js"), "needle").unwrap();
        fs::write(dir.path().join("top.txt"), "needle").unwrap();

        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "needle".into(),
            ..Default::default()
        };
        let mut engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
        let (results, _) = engine.search();
        assert_eq!(results.len(), 1);
        assert!(results[0].path.to_string_lossy().ends_with("top.txt"));
    }

    #[test]
    fn test_limit_truncates_results() {
        let dir = tempdir().unwrap();
        for i in 0..5 {
            fs::write(dir.path().join(format!("f{i}.txt")), "needle").unwrap();
        }
        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "needle".into(),
            limit: 2,
            ..Default::default()
        };
        let mut engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
        let (results, _) = engine.search();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_save_and_use_index_roundtrip() {
        let search_dir = tempdir().unwrap();
        let index_dir = tempdir().unwrap();
        fs::write(search_dir.path().join("a.txt"), "hello world").unwrap();
        let index_path = index_dir.path().join("my_index.json");

        let config = SearchConfig {
            directory: search_dir.path().to_path_buf(),
            pattern: "hello".into(),
            ..Default::default()
        };
        let index_config = IndexConfig {
            save_index: true,
            use_index: false,
            index_file: Some(index_path.clone()),
        };
        let mut engine = SearchEngine::new(config.clone(), index_config).unwrap();
        let (results, _) = engine.search();
        assert_eq!(results.len(), 1);
        assert!(index_path.exists());

        let index_config = IndexConfig {
            save_index: false,
            use_index: true,
            index_file: Some(index_path.clone()),
        };
        let mut engine = SearchEngine::new(config, index_config).unwrap();
        let (results, _) = engine.search();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_stats_recorded() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "needle").unwrap();
        fs::write(dir.path().join("b.txt"), "other").unwrap();
        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "needle".into(),
            ..Default::default()
        };
        let mut engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
        let (_results, stats) = engine.search();
        assert_eq!(stats.files_scanned, 2);
        assert_eq!(stats.files_matched, 1);
        assert_eq!(stats.total_matches, 1);
    }

    #[test]
    fn test_extraction_failure_counted_as_skipped() {
        // Invalid PDF that pdf-extract cannot parse: this triggers the
        // `SearchResult::with_error` branch inside the search engine and
        // the matching `inc_skipped` path in the stats aggregator.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("broken.pdf"), b"this is not a PDF").unwrap();
        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "whatever".into(),
            ..Default::default()
        };
        let mut engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
        let (results, stats) = engine.search();
        assert!(results.is_empty());
        assert_eq!(stats.files_skipped, 1);
    }

    #[test]
    fn test_index_rebuild_when_load_fails() {
        // When save_index is true but the existing index file is corrupt,
        // a fresh index should still be produced and saved.
        let search_dir = tempdir().unwrap();
        fs::write(search_dir.path().join("a.txt"), "needle here").unwrap();
        let index_path = search_dir.path().join("corrupt.json");
        fs::write(&index_path, "not valid json at all").unwrap();

        let config = SearchConfig {
            directory: search_dir.path().to_path_buf(),
            pattern: "needle".into(),
            ..Default::default()
        };
        let index_config = IndexConfig {
            save_index: true,
            use_index: false,
            index_file: Some(index_path.clone()),
        };
        let mut engine = SearchEngine::new(config, index_config).unwrap();
        let (results, _) = engine.search();
        assert_eq!(results.len(), 1);
        // The corrupt file should have been replaced with a valid index.
        let contents = fs::read_to_string(&index_path).unwrap();
        assert!(contents.contains("needle here") || contents.contains("a.txt"));
    }

    #[test]
    fn test_find_matches_literal_direct() {
        let config = SearchConfig {
            directory: PathBuf::from("."),
            pattern: "foo".into(),
            case_sensitive: false,
            ..Default::default()
        };
        let engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
        let matches = engine.find_matches("foo FOO Foo bar");
        assert_eq!(matches.len(), 3);
    }

    #[test]
    fn test_find_matches_regex_direct() {
        let config = SearchConfig {
            directory: PathBuf::from("."),
            pattern: r"\d+".into(),
            use_regex: true,
            ..Default::default()
        };
        let engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
        let matches = engine.find_matches("abc 123 def 456");
        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn test_multiple_files_sorted_by_match_count() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("one.txt"), "needle").unwrap();
        fs::write(dir.path().join("many.txt"), "needle needle needle\nneedle").unwrap();

        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "needle".into(),
            ..Default::default()
        };
        let mut engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
        let (results, _) = engine.search();
        assert_eq!(results.len(), 2);
        assert!(results[0].path.to_string_lossy().ends_with("many.txt"));
    }

    // ---- Progress handle + quiet mode -------------------------------------

    #[test]
    fn progress_handle_reports_total_and_current_after_search() {
        let dir = tempdir().unwrap();
        for i in 0..4 {
            fs::write(dir.path().join(format!("f{i}.txt")), "needle").unwrap();
        }
        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "needle".into(),
            ..Default::default()
        };
        let mut engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
        let progress = engine.progress_handle();
        let (_results, _stats) = engine.search();
        // After search completes, total and current must reflect the scan.
        assert_eq!(progress.total.load(Ordering::Relaxed), 4);
        assert_eq!(progress.current.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn progress_handle_resets_between_runs() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "needle").unwrap();
        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "needle".into(),
            ..Default::default()
        };
        let mut engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
        let progress = engine.progress_handle();

        let _ = engine.search();
        let after_first = progress.current.load(Ordering::Relaxed);
        assert!(after_first > 0);

        // A second search on the same engine must start from zero.
        let _ = engine.search();
        assert_eq!(
            progress.current.load(Ordering::Relaxed),
            after_first,
            "second run should end at the same total as the first"
        );
    }

    #[test]
    fn set_quiet_is_respected_without_breaking_results() {
        // Not much to assert on stderr output (it's captured by the test
        // harness), but we can at least verify that turning quiet on doesn't
        // break the search itself.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "needle").unwrap();
        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "needle".into(),
            ..Default::default()
        };
        let mut engine = SearchEngine::new(config, IndexConfig::default()).unwrap();
        engine.set_quiet(true);
        let (results, _) = engine.search();
        assert_eq!(results.len(), 1);
    }

    // ---- Index-file exclusion ---------------------------------------------

    #[test]
    fn index_file_is_never_included_in_results() {
        // Save an index, then search for a term that appears inside the
        // index JSON. The index file itself must not show up as a match.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("hit.txt"), "distinctive-token payload").unwrap();

        let save_cfg = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "distinctive-token".into(),
            ..Default::default()
        };
        let index_config = IndexConfig {
            save_index: true,
            use_index: false,
            index_file: None, // default .argus_index.json in the search dir
        };
        let mut engine = SearchEngine::new(save_cfg.clone(), index_config.clone()).unwrap();
        let (results, _) = engine.search();
        assert_eq!(results.len(), 1);
        assert!(dir.path().join(".argus_index.json").exists());

        // Second run: with --hidden the index file would normally be scanned.
        // The dedicated exclusion must still remove it.
        let second_cfg = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "distinctive-token".into(),
            include_hidden: true,
            ..Default::default()
        };
        let mut engine = SearchEngine::new(second_cfg, IndexConfig::default()).unwrap();
        let (results, _) = engine.search();
        let paths: Vec<String> = results
            .iter()
            .map(|r| r.path.to_string_lossy().to_string())
            .collect();
        assert!(
            !paths.iter().any(|p| p.ends_with(".argus_index.json")),
            "index file should be excluded even with --hidden, got: {paths:?}",
        );
    }

    #[test]
    fn custom_index_file_path_is_also_excluded() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "token-x content").unwrap();
        let custom_index = dir.path().join("my_custom.json");

        let save_cfg = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "token-x".into(),
            ..Default::default()
        };
        let index_config = IndexConfig {
            save_index: true,
            use_index: false,
            index_file: Some(custom_index.clone()),
        };
        let mut engine = SearchEngine::new(save_cfg, index_config.clone()).unwrap();
        let _ = engine.search();
        assert!(custom_index.exists());

        // Now search again with the same custom-index config — the file
        // must not appear in the results.
        let config = SearchConfig {
            directory: dir.path().to_path_buf(),
            pattern: "token-x".into(),
            ..Default::default()
        };
        let mut engine = SearchEngine::new(config, index_config).unwrap();
        let (results, _) = engine.search();
        let paths: Vec<String> = results
            .iter()
            .map(|r| r.path.to_string_lossy().to_string())
            .collect();
        assert!(
            !paths.iter().any(|p| p.ends_with("my_custom.json")),
            "custom index file should be excluded, got: {paths:?}",
        );
    }
}
