//! Search engine with parallel processing.

use crate::extractors::{extract_text, is_binary_file};
use crate::types::{FileType, Match, SearchConfig, SearchResult, SearchStats};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use regex::{Regex, RegexBuilder};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use walkdir::{DirEntry, WalkDir};

/// The search engine that coordinates file discovery and text matching.
pub struct SearchEngine {
    config: SearchConfig,
    pattern: SearchPattern,
}

/// Compiled search pattern (either regex or literal).
enum SearchPattern {
    Regex(Regex),
    Literal { pattern: String, lowercase: String },
}

impl SearchEngine {
    /// Create a new search engine with the given configuration.
    pub fn new(config: SearchConfig) -> Result<Self, regex::Error> {
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

        Ok(Self { config, pattern })
    }

    /// Execute the search and return results.
    pub fn search(&self) -> (Vec<SearchResult>, SearchStats) {
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

        // Process files in parallel using rayon
        files.par_iter().for_each(|file_path| {
            let result = self.search_file(file_path);

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

    /// Search a single file for matches.
    fn search_file(&self, path: &PathBuf) -> Option<SearchResult> {
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
                path.clone(),
                file_type,
                extraction.error.unwrap_or_else(|| "Unknown error".to_string()),
            ));
        }

        // Search for matches
        let matches = self.find_matches(&extraction.text);

        if matches.is_empty() {
            None
        } else {
            Some(SearchResult::new(path.clone(), file_type, matches, file_size))
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

        let engine = SearchEngine::new(config).unwrap();
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

        let engine = SearchEngine::new(config).unwrap();
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

        let engine = SearchEngine::new(config).unwrap();
        let (results, _) = engine.search();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].matches.len(), 3);
    }
}
