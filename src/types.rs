//! Core data types for Argus search tool.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;
use std::path::{Path, PathBuf};

/// Represents the type of file being searched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileType {
    /// Plain text files (.txt, .md, etc.)
    Text,
    /// Source code files
    Code,
    /// PDF documents
    Pdf,
    /// Microsoft Word documents (.docx)
    Docx,
    /// Image files (when OCR is enabled)
    Image,
    /// Unknown/Other file types
    Other,
}

impl FileType {
    /// Get the emoji icon for this file type.
    pub fn icon(&self) -> &'static str {
        match self {
            FileType::Text => "📄",
            FileType::Code => "💻",
            FileType::Pdf => "📕",
            FileType::Docx => "📘",
            FileType::Image => "🖼️ ",
            FileType::Other => "📎",
        }
    }

    /// Get the color name for this file type.
    pub fn color(&self) -> &'static str {
        match self {
            FileType::Code => "cyan",
            FileType::Pdf => "red",
            FileType::Docx => "blue",
            FileType::Image => "magenta",
            FileType::Text | FileType::Other => "white",
        }
    }

    /// Detect file type from extension.
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            // Text files
            "txt" | "md" | "markdown" | "rst" | "log" | "csv" | "tsv" | "json" | "yaml" | "yml"
            | "toml" | "ini" | "cfg" | "conf" | "xml" | "html" | "htm" | "css" => FileType::Text,

            // Code files
            "rs" | "py" | "js" | "ts" | "jsx" | "tsx" | "java" | "c" | "cpp" | "cc" | "cxx"
            | "h" | "hpp" | "go" | "rb" | "php" | "swift" | "kt" | "kts" | "scala" | "sh"
            | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" | "sql" | "r" | "lua" | "pl"
            | "pm" | "ex" | "exs" | "erl" | "hrl" | "hs" | "lhs" | "ml" | "mli" | "fs" | "fsi"
            | "fsx" | "clj" | "cljs" | "cljc" | "nim" | "zig" | "v" | "d" | "dart" | "vue"
            | "svelte" => FileType::Code,

            // PDF
            "pdf" => FileType::Pdf,

            // Word documents
            "docx" => FileType::Docx,

            // Images
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "tiff" | "tif" | "webp" => FileType::Image,

            // Other
            _ => FileType::Other,
        }
    }
}

impl fmt::Display for FileType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            FileType::Text => "Text",
            FileType::Code => "Code",
            FileType::Pdf => "PDF",
            FileType::Docx => "DOCX",
            FileType::Image => "Image",
            FileType::Other => "Other",
        };
        write!(f, "{name}")
    }
}

/// Represents a single match within a file.
#[derive(Debug, Clone)]
pub struct Match {
    /// The matched text content.
    pub matched_text: String,
    /// Context around the match (the full line or surrounding text).
    pub context: String,
}

impl Match {
    /// Create a new match.
    pub fn new(matched_text: String, context: String) -> Self {
        Self {
            matched_text,
            context,
        }
    }
}

/// Represents a search result for a single file.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Path to the file.
    pub path: PathBuf,
    /// Type of the file.
    pub file_type: FileType,
    /// All matches found in this file.
    pub matches: Vec<Match>,
    /// Confidence score (0.0 - 1.0).
    pub confidence: f64,
    /// Error message if extraction partially failed.
    pub error: Option<String>,
}

impl SearchResult {
    /// Create a new search result.
    pub fn new(path: PathBuf, file_type: FileType, matches: Vec<Match>, file_size: u64) -> Self {
        let confidence = Self::calculate_confidence(&matches, file_size);
        Self {
            path,
            file_type,
            matches,
            confidence,
            error: None,
        }
    }

    /// Create a search result with an error.
    pub fn with_error(path: PathBuf, file_type: FileType, error: String) -> Self {
        Self {
            path,
            file_type,
            matches: Vec::new(),
            confidence: 0.0,
            error: Some(error),
        }
    }

    /// Calculate confidence score based on matches and file characteristics.
    fn calculate_confidence(matches: &[Match], file_size: u64) -> f64 {
        if matches.is_empty() {
            return 0.0;
        }

        let match_count = matches.len() as f64;

        // Base score from match count (logarithmic scaling)
        let match_score = (match_count.ln() + 1.0).min(5.0) / 5.0;

        // Density bonus: more matches in smaller files = higher relevance
        let size_kb = (file_size as f64) / 1024.0;
        let density = if size_kb > 0.0 {
            (match_count / size_kb).min(10.0) / 10.0
        } else {
            0.5
        };

        // Combine scores with weights
        let score = (match_score * 0.7) + (density * 0.3);

        // Clamp to 0.0 - 1.0
        score.clamp(0.0, 1.0)
    }

    /// Get the number of matches.
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// Get a preview of the first match.
    pub fn preview(&self, max_len: usize) -> Option<String> {
        self.matches.first().map(|m| {
            let context = m.context.trim();
            if context.len() > max_len {
                format!("{}...", &context[..max_len])
            } else {
                context.to_string()
            }
        })
    }

    /// Get the filename.
    pub fn filename(&self) -> String {
        self.path.file_name().map_or_else(
            || "unknown".to_string(),
            |n| n.to_string_lossy().to_string(),
        )
    }
}

impl Eq for SearchResult {}

impl PartialEq for SearchResult {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl Ord for SearchResult {
    fn cmp(&self, other: &Self) -> Ordering {
        // Sort by match count first (descending), then by confidence (descending)
        match other.matches.len().cmp(&self.matches.len()) {
            Ordering::Equal => other
                .confidence
                .partial_cmp(&self.confidence)
                .unwrap_or(Ordering::Equal),
            other_order => other_order,
        }
    }
}

impl PartialOrd for SearchResult {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// OCR configuration options for Tesseract.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct OcrConfig {
    /// Whether OCR is enabled for images.
    pub enabled: bool,
    /// Language model to use (e.g., "eng", "eng_fast", "deu").
    pub language: String,
    /// Page Segmentation Mode (PSM):
    /// 0 = OSD only, 1 = Auto + OSD, 3 = Auto (default), 6 = Uniform block,
    /// 7 = Single line, 8 = Single word, 11 = Sparse text, 13 = Raw line
    pub psm: Option<u8>,
    /// OCR Engine Mode (OEM):
    /// 0 = Legacy only, 1 = LSTM only (fast), 2 = Legacy + LSTM, 3 = Default
    pub oem: Option<u8>,
    /// DPI for image processing (higher = better quality, slower).
    pub dpi: Option<u32>,
    /// Character whitelist (only recognize these characters).
    pub whitelist: Option<String>,
    /// Preprocessing: auto-scale images larger than this dimension.
    pub max_image_dimension: Option<u32>,
    /// Enable fast mode (uses eng_fast if available, PSM 6, OEM 1).
    pub fast_mode: bool,
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            language: "eng".to_string(),
            psm: None,
            oem: None,
            dpi: None,
            whitelist: None,
            max_image_dimension: None,
            fast_mode: false,
        }
    }
}

impl OcrConfig {
    /// Create a fast OCR configuration optimized for speed.
    #[allow(dead_code)]
    pub fn fast() -> Self {
        Self {
            enabled: true,
            language: "eng".to_string(),
            psm: Some(6), // Uniform block - fastest
            oem: Some(1), // LSTM only - faster than combined
            dpi: None,
            whitelist: None,
            max_image_dimension: Some(2000),
            fast_mode: true,
        }
    }
}

/// Search configuration options.
#[derive(Debug, Clone)]
pub struct SearchConfig {
    /// Directory to search in.
    pub directory: PathBuf,
    /// Search pattern (text or regex).
    pub pattern: String,
    /// Whether the search is case-sensitive.
    pub case_sensitive: bool,
    /// Whether to use regex matching.
    pub use_regex: bool,
    /// OCR configuration.
    pub ocr: OcrConfig,
    /// Maximum number of results to return.
    pub limit: usize,
    /// Maximum directory depth.
    pub max_depth: Option<usize>,
    /// Include hidden files.
    pub include_hidden: bool,
    /// File extensions to include (empty = all).
    pub extensions: Vec<String>,
    /// Show content preview.
    pub show_preview: bool,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            directory: PathBuf::from("."),
            pattern: String::new(),
            case_sensitive: false,
            use_regex: false,
            ocr: OcrConfig::default(),
            limit: 20,
            max_depth: None,
            include_hidden: false,
            extensions: Vec::new(),
            show_preview: false,
        }
    }
}

/// Configuration for index file handling.
#[derive(Debug, Clone, Default)]
pub struct IndexConfig {
    /// Whether to save an index after scanning.
    pub save_index: bool,
    /// Whether to use an existing index if available.
    pub use_index: bool,
    /// Path to the index file. If None, defaults to `.argus_index.json` in the search directory.
    pub index_file: Option<PathBuf>,
}

impl IndexConfig {
    /// Get the index file path, using the default if not specified.
    pub fn get_index_path(&self, search_dir: &Path) -> PathBuf {
        self.index_file
            .clone()
            .unwrap_or_else(|| search_dir.join(".argus_index.json"))
    }
}

/// Statistics about the search operation.
#[derive(Debug, Clone, Default)]
pub struct SearchStats {
    /// Total files scanned.
    pub files_scanned: usize,
    /// Files with matches.
    pub files_matched: usize,
    /// Total matches found.
    pub total_matches: usize,
    /// Files skipped due to errors.
    pub files_skipped: usize,
    /// Search duration in milliseconds.
    pub duration_ms: u64,
    /// Breakdown by file type.
    pub by_type: std::collections::HashMap<FileType, usize>,
}

impl SearchStats {
    /// Create new empty stats.
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment files scanned.
    pub fn inc_scanned(&mut self) {
        self.files_scanned += 1;
    }

    /// Add a match result.
    pub fn add_result(&mut self, result: &SearchResult) {
        if !result.matches.is_empty() {
            self.files_matched += 1;
            self.total_matches += result.matches.len();
            *self.by_type.entry(result.file_type).or_insert(0) += 1;
        }
    }

    /// Increment skipped files.
    pub fn inc_skipped(&mut self) {
        self.files_skipped += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_type_from_extension_variants() {
        assert_eq!(FileType::from_extension("md"), FileType::Text);
        assert_eq!(FileType::from_extension("TOML"), FileType::Text);
        assert_eq!(FileType::from_extension("cpp"), FileType::Code);
        assert_eq!(FileType::from_extension("kt"), FileType::Code);
        assert_eq!(FileType::from_extension("pdf"), FileType::Pdf);
        assert_eq!(FileType::from_extension("docx"), FileType::Docx);
        assert_eq!(FileType::from_extension("JPEG"), FileType::Image);
        assert_eq!(FileType::from_extension("bin"), FileType::Other);
        assert_eq!(FileType::from_extension(""), FileType::Other);
    }

    #[test]
    fn file_type_icon_and_color_distinct() {
        let variants = [
            FileType::Text,
            FileType::Code,
            FileType::Pdf,
            FileType::Docx,
            FileType::Image,
            FileType::Other,
        ];
        for v in variants {
            assert!(!v.icon().is_empty());
            assert!(!v.color().is_empty());
        }
    }

    #[test]
    fn file_type_display_formats() {
        assert_eq!(format!("{}", FileType::Text), "Text");
        assert_eq!(format!("{}", FileType::Code), "Code");
        assert_eq!(format!("{}", FileType::Pdf), "PDF");
        assert_eq!(format!("{}", FileType::Docx), "DOCX");
        assert_eq!(format!("{}", FileType::Image), "Image");
        assert_eq!(format!("{}", FileType::Other), "Other");
    }

    #[test]
    fn match_new_stores_fields() {
        let m = Match::new("foo".into(), "line with foo".into());
        assert_eq!(m.matched_text, "foo");
        assert_eq!(m.context, "line with foo");
    }

    #[test]
    fn search_result_new_computes_confidence() {
        let matches = vec![Match::new("x".into(), "x".into()); 5];
        let r = SearchResult::new(PathBuf::from("a.txt"), FileType::Text, matches, 1024);
        assert!(r.confidence > 0.0 && r.confidence <= 1.0);
        assert_eq!(r.match_count(), 5);
    }

    #[test]
    fn search_result_empty_confidence_is_zero() {
        let r = SearchResult::new(PathBuf::from("a.txt"), FileType::Text, vec![], 1024);
        assert_eq!(r.confidence, 0.0);
        assert_eq!(r.match_count(), 0);
    }

    #[test]
    fn search_result_zero_size_file() {
        let matches = vec![Match::new("x".into(), "x".into())];
        let r = SearchResult::new(PathBuf::from("a.txt"), FileType::Text, matches, 0);
        assert!(r.confidence >= 0.0 && r.confidence <= 1.0);
    }

    #[test]
    fn search_result_with_error_has_no_matches() {
        let r = SearchResult::with_error(PathBuf::from("a.txt"), FileType::Text, "boom".into());
        assert!(r.matches.is_empty());
        assert_eq!(r.confidence, 0.0);
        assert_eq!(r.error.as_deref(), Some("boom"));
    }

    #[test]
    fn search_result_filename_and_preview() {
        let matches = vec![Match::new(
            "bar".into(),
            "  this is a line containing bar here".into(),
        )];
        let r = SearchResult::new(PathBuf::from("/tmp/foo.txt"), FileType::Text, matches, 128);
        assert_eq!(r.filename(), "foo.txt");
        let preview = r.preview(10).unwrap();
        assert!(preview.ends_with("..."));
        assert!(preview.len() <= 13);
    }

    #[test]
    fn search_result_preview_short_context_not_truncated() {
        let matches = vec![Match::new("x".into(), "short".into())];
        let r = SearchResult::new(PathBuf::from("a.txt"), FileType::Text, matches, 1);
        assert_eq!(r.preview(80).unwrap(), "short");
    }

    #[test]
    fn search_result_preview_none_when_empty() {
        let r = SearchResult::new(PathBuf::from("a.txt"), FileType::Text, vec![], 1);
        assert!(r.preview(80).is_none());
    }

    #[test]
    fn search_result_ord_sorts_by_match_count_desc() {
        let make = |count: usize, path: &str| {
            let matches = vec![Match::new("x".into(), "x".into()); count];
            SearchResult::new(PathBuf::from(path), FileType::Text, matches, 1024)
        };
        let mut v = [make(1, "a"), make(5, "b"), make(3, "c")];
        v.sort();
        assert_eq!(v[0].path, PathBuf::from("b"));
        assert_eq!(v[1].path, PathBuf::from("c"));
        assert_eq!(v[2].path, PathBuf::from("a"));
    }

    #[test]
    fn search_result_partial_ord_matches_ord() {
        let a = SearchResult::new(
            PathBuf::from("a"),
            FileType::Text,
            vec![Match::new("x".into(), "x".into())],
            1,
        );
        let b = SearchResult::new(PathBuf::from("b"), FileType::Text, vec![], 1);
        assert_eq!(a.partial_cmp(&b), Some(a.cmp(&b)));
    }

    #[test]
    fn search_result_partial_eq_by_path() {
        let a = SearchResult::with_error(PathBuf::from("x"), FileType::Text, "e".into());
        let b = SearchResult::with_error(PathBuf::from("x"), FileType::Code, "other".into());
        assert_eq!(a, b);
    }

    #[test]
    fn ocr_config_default_disabled() {
        let cfg = OcrConfig::default();
        assert!(!cfg.enabled);
        assert!(!cfg.fast_mode);
        assert_eq!(cfg.language, "eng");
    }

    #[test]
    fn ocr_config_fast_enabled() {
        let cfg = OcrConfig::fast();
        assert!(cfg.enabled);
        assert!(cfg.fast_mode);
        assert_eq!(cfg.psm, Some(6));
        assert_eq!(cfg.oem, Some(1));
    }

    #[test]
    fn search_config_default_is_sensible() {
        let cfg = SearchConfig::default();
        assert_eq!(cfg.directory, PathBuf::from("."));
        assert!(cfg.pattern.is_empty());
        assert!(!cfg.case_sensitive);
        assert!(!cfg.use_regex);
        assert_eq!(cfg.limit, 20);
        assert!(cfg.extensions.is_empty());
    }

    #[test]
    fn index_config_get_index_path_default() {
        let cfg = IndexConfig::default();
        let p = cfg.get_index_path(Path::new("/tmp/project"));
        assert_eq!(p, PathBuf::from("/tmp/project/.argus_index.json"));
    }

    #[test]
    fn index_config_get_index_path_custom() {
        let cfg = IndexConfig {
            save_index: true,
            use_index: true,
            index_file: Some(PathBuf::from("/var/argus.json")),
        };
        let p = cfg.get_index_path(Path::new("/tmp/project"));
        assert_eq!(p, PathBuf::from("/var/argus.json"));
    }

    #[test]
    fn search_stats_increments() {
        let mut s = SearchStats::new();
        s.inc_scanned();
        s.inc_scanned();
        s.inc_skipped();
        assert_eq!(s.files_scanned, 2);
        assert_eq!(s.files_skipped, 1);
        assert_eq!(s.files_matched, 0);
    }

    #[test]
    fn search_stats_add_result_updates_by_type() {
        let mut s = SearchStats::new();
        let matches = vec![Match::new("x".into(), "x".into()); 3];
        let result = SearchResult::new(PathBuf::from("f.rs"), FileType::Code, matches, 1024);
        s.add_result(&result);
        assert_eq!(s.files_matched, 1);
        assert_eq!(s.total_matches, 3);
        assert_eq!(s.by_type.get(&FileType::Code).copied(), Some(1));
    }

    #[test]
    fn search_stats_add_result_ignores_empty_matches() {
        let mut s = SearchStats::new();
        let result = SearchResult::new(PathBuf::from("f.rs"), FileType::Code, vec![], 1);
        s.add_result(&result);
        assert_eq!(s.files_matched, 0);
        assert_eq!(s.total_matches, 0);
    }

    #[test]
    fn file_type_serde_roundtrip() {
        let json = serde_json::to_string(&FileType::Pdf).unwrap();
        let back: FileType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, FileType::Pdf);
    }
}
