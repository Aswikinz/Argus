//! Argus - The All-Seeing File Search Tool
//!
//! A powerful CLI tool for searching text across any file format,
//! including PDFs, Word documents, images (with OCR), and code files.

use clap::{Parser, ValueEnum, ValueHint};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process;

use argus::search::SearchEngine;
use argus::types::{IndexConfig, OcrConfig, OcrEngine, SearchConfig, VisionLlmConfig};
use argus::ui::{
    display_banner, display_error, display_farewell, display_results, flush, interactive_select,
    open_file,
};

/// CLI-facing enum for OCR backend selection. Maps onto [`OcrEngine`].
#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliOcrEngine {
    /// Tesseract via leptess. Fast, low memory, needs the `ocr` feature.
    Tesseract,
    /// ocrs ONNX engine. Higher accuracy on modern docs; needs the `ocrs`
    /// feature. Downloads ~25 MB of models on first use.
    Ocrs,
    /// Vision-LLM over HTTP (OpenAI-compatible). Defaults to Ollama +
    /// `glm-ocr` locally. Dramatic accuracy gains on handwriting,
    /// newspapers, and rotated scans. Needs the `vision-llm` feature.
    #[value(name = "vision-llm")]
    VisionLlm,
}

impl From<CliOcrEngine> for OcrEngine {
    fn from(value: CliOcrEngine) -> Self {
        match value {
            CliOcrEngine::Tesseract => OcrEngine::Tesseract,
            CliOcrEngine::Ocrs => OcrEngine::Ocrs,
            CliOcrEngine::VisionLlm => OcrEngine::VisionLlm,
        }
    }
}

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
                  argus -o --ocr-engine vision-llm \"handwriting\"\n    \
                  \\                              Use Ollama + glm-ocr for handwriting / newspapers\n    \
                  argus -o --ocr-engine vision-llm --ocr-endpoint https://api.openai.com/v1/chat/completions\n    \
                  \\    --ocr-model gpt-4o-mini \"invoice\"   Use a cloud provider (ARGUS_OCR_API_KEY)\n    \
                  argus -s -l 50 \"Error\"          Case-sensitive, limit to 50 results\n    \
                  argus -i \"pattern\"              Save index for faster future searches\n    \
                  argus -I \"pattern\"              Use existing index if available\n    \
                  argus -iI \"pattern\"             Use index and update it with new files"
)]
struct Cli {
    /// The search pattern (text or regex with -r flag). Omit to launch the
    /// interactive TUI where you can compose a search visually.
    pattern: Option<String>,

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

    /// Enable OCR for images and scanned PDFs
    #[arg(short = 'o', long = "ocr")]
    ocr: bool,

    /// OCR backend to use when --ocr is enabled
    #[arg(long = "ocr-engine", value_enum, default_value_t = default_cli_engine())]
    ocr_engine: CliOcrEngine,

    /// Vision-LLM endpoint (OpenAI-compatible chat-completions URL). Used only
    /// when --ocr-engine=vision-llm. Defaults to a local Ollama instance.
    #[arg(
        long = "ocr-endpoint",
        default_value = "http://localhost:11434/v1/chat/completions",
        value_name = "URL"
    )]
    ocr_endpoint: String,

    /// Vision-LLM model name. Used only when --ocr-engine=vision-llm.
    /// The default targets Ollama's `glm-ocr` — the 0.9 B OCR-specialised model
    /// that tops OmniDocBench. Other examples: qwen2.5vl:7b, minicpm-v:8b,
    /// gpt-4o-mini, claude-sonnet-4-5.
    #[arg(long = "ocr-model", default_value = "glm-ocr", value_name = "NAME")]
    ocr_model: String,

    /// Instruction prompt sent to the vision LLM. Override for tables, math,
    /// specific languages, etc.
    #[arg(
        long = "ocr-prompt",
        default_value = "Extract all visible text from this image. Preserve line breaks and reading order. Return only the text — no commentary, no formatting, no explanations.",
        value_name = "TEXT"
    )]
    ocr_prompt: String,

    /// Per-request timeout for the vision-LLM backend, in seconds.
    #[arg(long = "ocr-timeout", default_value_t = 120, value_name = "SECS")]
    ocr_timeout: u64,

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

    // Assemble vision-LLM config up front so both the CLI path and the TUI
    // Prefill see the same values. API key is sourced from the environment
    // so it never appears in argv or shell history.
    let vision_llm_config = VisionLlmConfig {
        endpoint: cli.ocr_endpoint.clone(),
        model: cli.ocr_model.clone(),
        api_key: std::env::var("ARGUS_OCR_API_KEY")
            .ok()
            .filter(|s| !s.is_empty()),
        prompt: cli.ocr_prompt.clone(),
        timeout_secs: cli.ocr_timeout,
    };

    // Check OCR availability
    if cli.ocr {
        match cli.ocr_engine {
            CliOcrEngine::Tesseract => {
                #[cfg(not(feature = "ocr"))]
                eprintln!("  warning: tesseract OCR not compiled. Rebuild with --features ocr");
            }
            CliOcrEngine::Ocrs => {
                #[cfg(not(feature = "ocrs"))]
                eprintln!("  warning: ocrs OCR not compiled. Rebuild with --features ocrs");

                // Eagerly initialize the ocrs engine on the main thread BEFORE
                // the parallel search starts. Otherwise the first rayon worker
                // to hit an image blocks every other worker on the OnceLock
                // while it streams the models from S3, which looks like a
                // hang behind the progress bar.
                #[cfg(feature = "ocrs")]
                if let Err(e) = argus::ocrs_backend::ensure_ready() {
                    display_error(&format!("ocrs unavailable: {e}"));
                    process::exit(1);
                }
            }
            CliOcrEngine::VisionLlm => {
                #[cfg(not(feature = "vision-llm"))]
                eprintln!(
                    "  warning: vision-llm OCR not compiled. Rebuild with --features vision-llm"
                );

                // Health-check the endpoint up front. Without this, a wrong
                // URL or an Ollama daemon that isn't running would surface as
                // a per-file timeout during the parallel scan — confusing and
                // slow. A fast fail here is much kinder.
                #[cfg(feature = "vision-llm")]
                if let Err(e) = argus::vision_llm_backend::ensure_ready(&vision_llm_config) {
                    display_error(&format!("vision-llm unavailable: {e}"));
                    process::exit(1);
                }
            }
        }
    }

    let directory = cli.directory.canonicalize().unwrap_or(cli.directory);

    // Build index configuration (shared by both modes).
    let index_config = IndexConfig {
        save_index: cli.save_index,
        use_index: cli.use_index,
        index_file: cli.index_file,
    };

    // If no pattern was given on the command line, launch the interactive TUI.
    // The TUI lets the user compose their query visually, run it, inspect
    // results, and loop back to compose another search — ideal for
    // non-technical users who don't want to memorise flags.
    //
    // On a non-interactive stdin/stdout (scripts, CI, piped input) the TUI
    // cannot run, so we surface the clap-style "pattern required" error
    // instead. That keeps existing automation working unchanged.
    let Some(pattern) = cli.pattern else {
        if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
            display_error(
                "a search pattern is required when stdin/stdout is not a terminal. \
                 run `argus <pattern>` or launch argus from an interactive shell.",
            );
            process::exit(2);
        }
        let prefill = argus::tui::Prefill {
            directory: directory.clone(),
            case_sensitive: cli.case_sensitive,
            use_regex: cli.regex,
            ocr_enabled: cli.ocr,
            ocr_engine: cli.ocr_engine.into(),
            vision_llm: vision_llm_config.clone(),
            limit: cli.limit,
            max_depth: cli.max_depth,
            include_hidden: cli.hidden,
            extensions: cli.extensions.clone().unwrap_or_default(),
            show_preview: cli.preview,
        };
        if let Err(e) = argus::tui::run(prefill, index_config) {
            display_error(&format!("TUI error: {e}"));
            process::exit(1);
        }
        #[cfg(feature = "ocr")]
        suppress_stderr();
        return;
    };

    if !cli.no_banner {
        display_banner();
    }

    // Build search configuration
    let config = SearchConfig {
        directory: directory.clone(),
        pattern,
        case_sensitive: cli.case_sensitive,
        use_regex: cli.regex,
        ocr: OcrConfig {
            enabled: cli.ocr,
            engine: cli.ocr_engine.into(),
            vision_llm: vision_llm_config,
            ..OcrConfig::default()
        },
        limit: cli.limit,
        max_depth: cli.max_depth,
        include_hidden: cli.hidden,
        extensions: cli.extensions.unwrap_or_default(),
        show_preview: cli.preview,
    };

    // Create search engine
    let mut engine = match SearchEngine::new(config.clone(), index_config) {
        Ok(e) => e,
        Err(e) => {
            display_error(&format!("Invalid regex pattern: {e}"));
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
                    display_error(&format!("Failed to open file: {e}"));
                }
                // Continue the loop to allow selecting another file
                println!();
            } else {
                display_farewell();
                break;
            }
        }
    }

    // Suppress Tesseract cleanup warnings by redirecting stderr before exit
    #[cfg(feature = "ocr")]
    suppress_stderr();
}

/// The default OCR engine shown in `--help`, derived from compile features.
///
/// Preference order: Tesseract → Ocrs → VisionLlm. We deliberately only
/// auto-select VisionLlm as the default when neither CPU backend is
/// compiled, so users who've built with `--features ocr` keep their
/// existing behaviour unchanged.
fn default_cli_engine() -> CliOcrEngine {
    #[cfg(feature = "ocr")]
    {
        CliOcrEngine::Tesseract
    }
    #[cfg(all(not(feature = "ocr"), feature = "ocrs"))]
    {
        CliOcrEngine::Ocrs
    }
    #[cfg(all(not(feature = "ocr"), not(feature = "ocrs"), feature = "vision-llm"))]
    {
        CliOcrEngine::VisionLlm
    }
    #[cfg(not(any(feature = "ocr", feature = "ocrs", feature = "vision-llm")))]
    {
        CliOcrEngine::Tesseract
    }
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
