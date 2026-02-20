//! Argus - The All-Seeing File Search Tool
//!
//! A powerful CLI tool for searching text across any file format,
//! including PDFs, Word documents, images (with OCR), and code files.

mod extractors;
mod index;
mod search;
mod types;
mod ui;

use clap::{Parser, ValueHint};
use std::path::PathBuf;
use std::process;

use search::SearchEngine;
use types::{IndexConfig, OcrConfig, SearchConfig};
use ui::{display_banner, display_error, display_results, flush, interactive_select, open_file};

/// Argus - The All-Seeing File Search Tool
///
/// Search across any file format: PDFs, Word docs, images (OCR), text, and code.
#[derive(Parser, Debug)]
#[command(
    name = "argus",
    author = "Argus Contributors",
    version,
    about = "👁️  Argus - The All-Seeing File Search Tool",
    long_about = "Search across any file format: PDFs, Word docs, images (OCR), text, and code.\n\n\
                  Named after Argus Panoptes, the all-seeing giant from Greek mythology.",
    after_help = "EXAMPLES:\n    \
                  argus \"TODO\"                    Search for TODO in current directory\n    \
                  argus -d ~/projects \"fn main\"   Search in specific directory\n    \
                  argus -r \"\\bfn\\s+\\w+\"           Use regex pattern matching\n    \
                  argus -e pdf,docx \"report\"      Search only in PDF and DOCX files\n    \
                  argus -o \"text in image\"        Enable OCR for images and scanned PDFs\n    \
                  argus -o -e pdf \"invoice\"       Search scanned PDF documents via OCR\n    \
                  argus -s -l 50 \"Error\"          Case-sensitive, limit to 50 results\n    \
                  argus -i \"pattern\"              Save index for faster future searches\n    \
                  argus -I \"pattern\"              Use existing index if available\n    \
                  argus -iI \"pattern\"             Use index and update it with new files"
)]
struct Cli {
    /// The search pattern (text or regex with -r flag)
    #[arg(required = true)]
    pattern: String,

    /// Directory to search in
    #[arg(
        short = 'd',
        long = "directory",
        value_hint = ValueHint::DirPath,
        default_value = "."
    )]
    directory: PathBuf,

    /// Maximum number of results to display
    #[arg(short = 'l', long = "limit", default_value = "20")]
    limit: usize,

    /// Enable case-sensitive search
    #[arg(short = 's', long = "case-sensitive")]
    case_sensitive: bool,

    /// Enable OCR for images and scanned PDFs (requires Tesseract)
    #[arg(short = 'o', long = "ocr")]
    ocr: bool,

    /// Use regex pattern matching
    #[arg(short = 'r', long = "regex")]
    regex: bool,

    /// Show content preview for each match
    #[arg(short = 'p', long = "preview")]
    preview: bool,

    /// Filter by file extensions (comma-separated, e.g., "pdf,txt,docx")
    #[arg(short = 'e', long = "extensions", value_delimiter = ',')]
    extensions: Option<Vec<String>>,

    /// Maximum directory depth to search
    #[arg(long = "max-depth")]
    max_depth: Option<usize>,

    /// Include hidden files and directories
    #[arg(short = 'H', long = "hidden")]
    hidden: bool,

    /// Suppress the banner
    #[arg(long = "no-banner", hide = true)]
    no_banner: bool,

    /// Non-interactive mode (just print results, don't prompt)
    #[arg(short = 'n', long = "non-interactive")]
    non_interactive: bool,

    /// Save index after scanning for faster future searches
    #[arg(short = 'i', long = "save-index")]
    save_index: bool,

    /// Use existing index if available (skip re-extraction for unchanged files)
    #[arg(short = 'I', long = "use-index")]
    use_index: bool,

    /// Path to index file (default: .argus_index.json in search directory)
    #[arg(long = "index-file", value_hint = ValueHint::FilePath)]
    index_file: Option<PathBuf>,
}

fn main() {
    // Parse command line arguments
    let cli = Cli::parse();

    // Display banner unless suppressed
    if !cli.no_banner {
        display_banner();
    }

    // Validate directory
    if !cli.directory.exists() {
        display_error(&format!(
            "Directory does not exist: {}",
            cli.directory.display()
        ));
        process::exit(1);
    }

    if !cli.directory.is_dir() {
        display_error(&format!(
            "Path is not a directory: {}",
            cli.directory.display()
        ));
        process::exit(1);
    }

    // Check OCR availability
    #[cfg(not(feature = "ocr"))]
    if cli.ocr {
        eprintln!(
            "  \x1b[33m⚠️  Warning: OCR feature not compiled. Rebuild with: cargo build --release --features ocr\x1b[0m"
        );
    }

    // Build search configuration
    let directory = cli.directory.canonicalize().unwrap_or(cli.directory);
    let config = SearchConfig {
        directory: directory.clone(),
        pattern: cli.pattern,
        case_sensitive: cli.case_sensitive,
        use_regex: cli.regex,
        ocr: OcrConfig {
            enabled: cli.ocr,
        },
        limit: cli.limit,
        max_depth: cli.max_depth,
        include_hidden: cli.hidden,
        extensions: cli.extensions.unwrap_or_default(),
        show_preview: cli.preview,
    };

    // Build index configuration
    let index_config = IndexConfig {
        save_index: cli.save_index,
        use_index: cli.use_index,
        index_file: cli.index_file,
    };

    // Create search engine
    let mut engine = match SearchEngine::new(config.clone(), index_config) {
        Ok(e) => e,
        Err(e) => {
            display_error(&format!("Invalid regex pattern: {}", e));
            process::exit(1);
        }
    };

    // Execute search
    let (results, stats) = engine.search();

    // Display results
    display_results(&results, &stats, config.show_preview);
    flush();

    // Skip interactive mode if non-interactive flag is set
    if cli.non_interactive {
        #[cfg(feature = "ocr")]
        suppress_stderr();
        return;
    }

    // Enter interactive selection mode
    if !results.is_empty() {
        loop {
            if let Some(selected) = interactive_select(&results) {
                if let Err(e) = open_file(selected) {
                    display_error(&format!("Failed to open file: {}", e));
                }
                // Continue the loop to allow selecting another file
                println!();
            } else {
                // User chose to exit
                println!("\n  {} Goodbye!\n", "👋".bright_white());
                break;
            }
        }
    }

    // Suppress Tesseract cleanup warnings by redirecting stderr before exit
    #[cfg(feature = "ocr")]
    suppress_stderr();
}

/// Redirect stderr to /dev/null to suppress third-party library warnings at exit.
#[cfg(feature = "ocr")]
fn suppress_stderr() {
    #[cfg(unix)]
    {
        use std::fs::File;
        use std::os::unix::io::AsRawFd;
        if let Ok(devnull) = File::open("/dev/null") {
            unsafe {
                libc::dup2(devnull.as_raw_fd(), 2);
            }
        }
    }
}

// Re-export for use with colored trait
use colored::Colorize;
