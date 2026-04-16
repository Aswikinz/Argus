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

/// The search engine that coordinates file discovery and text matching.
pub struct SearchEngine {
    config: SearchConfig,
    index_config: IndexConfig,
    pattern: SearchPattern,
    index: Option<Index>,
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

        // Try to load existing index if use_index is enabled
        let index = if index_config.use_index || index_config.save_index {
            let index_path = index_config.get_index_path(&config.directory);
            match Index::load(&index_path) {
                Ok(idx) => {
                    eprintln!("  \x1b[32m✓\x1b[0m Loaded index with {} entries", idx.len());
                    Some(idx)
                }
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
        })
    }

    /// Execute the search and return results.
    pub fn search(&mut self) -> (Vec<SearchResult>, SearchStats) {
        let start = Instant::now();

        // Collect all files to search
        let files = self.collect_files();
        let total_files = files.len();

        // Create progress bar
        let pb = ProgressBar::new(total_files as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {msg}")
                .unwrap()
                .progress_chars("█▓▒░  "),
        );
        pb.set_message("Searching...");

        // Thread-safe containers for results and stats
        let results: Arc<Mutex<Vec<SearchResult>>> = Arc::new(Mutex::new(Vec::new()));
        let stats = Arc::new(Mutex::new(SearchStats::new()));
        let files_processed = Arc::new(AtomicUsize::new(0));
        let new_index_entries: Arc<Mutex<Vec<IndexEntry>>> = Arc::new(Mutex::new(Vec::new()));

        // Clone index for thread-safe access
        let index_ref = self.index.as_ref().map(|i| Arc::new(i.clone()));
        let save_index = self.index_config.save_index;

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
            let processed = files_processed.fetch_add(1, Ordering::Relaxed) + 1;
            pb.set_position(processed as u64);
        });

        pb.finish_with_message("Search complete!");

        // Update index with new entries if save_index is enabled
        if self.index_config.save_index {
            if let Some(ref mut index) = self.index {
                let entries = Arc::try_unwrap(new_index_entries)
                    .map(|mutex| mutex.into_inner().unwrap())
                    .unwrap_or_else(|arc| arc.lock().unwrap().clone());

                for entry in entries {
                    index.upsert_entry(entry);
                }

                // Prune entries for files that no longer exist
                index.prune_missing();

                // Save the index
                let index_path = self.index_config.get_index_path(&self.config.directory);
                if let Err(e) = index.save(&index_path) {
                    eprintln!("  \x1b[33m⚠\x1b[0m Warning: Failed to save index: {}", e);
                } else {
                    eprintln!(
                        "  \x1b[32m✓\x1b[0m Saved index with {} entries to {}",
                        index.len(),
                        index_path.display()
                    );
                }
            }
        }

        // Get final results and stats
        let mut final_results = Arc::try_unwrap(results)
            .map(|mutex| mutex.into_inner().unwrap())
            .unwrap_or_else(|arc| arc.lock().unwrap().clone());
        let mut final_stats = Arc::try_unwrap(stats)
            .map(|mutex| mutex.into_inner().unwrap())
            .unwrap_or_else(|arc| arc.lock().unwrap().clone());

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

        walker
            .into_iter()
            .filter_entry(|e| self.should_process_entry(e))
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| {
                // Filter by extension if specified
                if extensions.is_empty() {
                    true
                } else {
                    e.path()
                        .extension()
                        .map(|ext| extensions.contains(&ext.to_string_lossy().to_lowercase()))
                        .unwrap_or(false)
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
                let extraction = extract_text(path, file_type, self.config.ocr.enabled);

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
            let extraction = extract_text(path, file_type, self.config.ocr.enabled);

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
        let file_size = path.metadata().map(|m| m.len()).unwrap_or(0);

        // Extract text
        let extraction = extract_text(path, file_type, self.config.ocr.enabled);

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
            fs::write(dir.path().join(format!("f{}.txt", i)), "needle").unwrap();
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
}
