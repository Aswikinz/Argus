//! Argus - The All-Seeing File Search Tool
//!
//! A powerful CLI tool for searching text across any file format,
//! including PDFs, Word documents, images (with OCR), and code files.

mod extractors;
mod search;
mod types;
mod ui;

use clap::{Parser, ValueHint};
use std::path::PathBuf;
use std::process;

use search::SearchEngine;
use types::SearchConfig;
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
                  argus -o \"text in image\"        Enable OCR for images\n    \
                  argus -s -l 50 \"Error\"          Case-sensitive, limit to 50 results"
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

    /// Enable OCR for searching text in images (requires Tesseract)
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
    let config = SearchConfig {
        directory: cli.directory.canonicalize().unwrap_or(cli.directory),
        pattern: cli.pattern,
        case_sensitive: cli.case_sensitive,
        use_regex: cli.regex,
        ocr_enabled: cli.ocr,
        limit: cli.limit,
        max_depth: cli.max_depth,
        include_hidden: cli.hidden,
        extensions: cli.extensions.unwrap_or_default(),
        show_preview: cli.preview,
    };

    // Create search engine
    let engine = match SearchEngine::new(config.clone()) {
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
