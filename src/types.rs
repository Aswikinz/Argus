//! Core data types for Argus search tool.

use std::cmp::Ordering;
use std::fmt;
use std::path::PathBuf;

/// Represents the type of file being searched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
            FileType::Text => "white",
            FileType::Code => "cyan",
            FileType::Pdf => "red",
            FileType::Docx => "blue",
            FileType::Image => "magenta",
            FileType::Other => "white",
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
        write!(f, "{}", name)
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
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string())
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
    /// Whether OCR is enabled for images.
    pub ocr_enabled: bool,
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
            ocr_enabled: false,
            limit: 20,
            max_depth: None,
            include_hidden: false,
            extensions: Vec::new(),
            show_preview: false,
        }
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
