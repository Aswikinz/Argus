# Argus - The All-Seeing File Search Tool

[![CI](https://github.com/Aswikinz/Argus/actions/workflows/ci.yml/badge.svg)](https://github.com/Aswikinz/Argus/actions/workflows/ci.yml)
[![Coverage](https://github.com/Aswikinz/Argus/actions/workflows/coverage.yml/badge.svg)](https://github.com/Aswikinz/Argus/actions/workflows/coverage.yml)
[![codecov](https://codecov.io/gh/Aswikinz/Argus/branch/main/graph/badge.svg)](https://codecov.io/gh/Aswikinz/Argus)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

Named after Argus Panoptes, the all-seeing giant from Greek mythology, **Argus** is a powerful CLI tool that searches for text across any file format.

## Features

- **Universal File Search**: Search through PDFs, Word documents (.docx), images (with OCR), text files, and code files
- **Interactive TUI**: Run `argus` with no arguments to open a full-screen dashboard — compose a query with chips for file types, toggles for every option, a live progress bar, and a results pane with preview. Loop from result back to a fresh search without leaving the app.
- **Fast Parallel Processing**: Leverages multi-core CPUs with Rayon for blazing-fast searches
- **Index Caching**: Save extracted text to an index file for instant subsequent searches
- **Beautiful CLI**: Colorful output with file type icons, confidence bars, and match highlighting
- **Interactive Selection**: Navigate results with arrow keys and open files instantly
- **Regex Support**: Full regex pattern matching when you need precise searches
- **Three OCR backends, pick your tradeoff**:
  - **Tesseract** — fast, native C++ via leptess (feature = `ocr`)
  - **ocrs** — pure-Rust ONNX engine, higher accuracy on clean modern docs (feature = `ocrs`)
  - **Vision-LLM** *(new!)* — any OpenAI-compatible vision model over HTTP (feature = `vision-llm`). Defaults to Ollama + [`glm-ocr`](https://ollama.com/library/glm-ocr) locally (~1 GB, #1 on OmniDocBench). Handles **handwriting, newspapers, and rotated/misaligned scans** — the three cases the CPU backends can't. Also speaks to OpenAI / Anthropic / Mistral / Groq / LM Studio for users without local GPUs.
- **Cross-Platform**: Works on Linux and Windows

## Installation

### From Source

```bash
# Clone the repository
git clone https://github.com/Aswikinz/Argus.git
cd argus

# Build without OCR (faster build, smaller binary)
cargo build --release

# Build with Tesseract OCR support (requires system Tesseract)
cargo build --release --features ocr

# Build with the pure-Rust ocrs ONNX backend (no Tesseract needed,
# downloads ~25 MB of models on first use)
cargo build --release --features ocrs

# Build with the vision-LLM OCR backend (for handwriting, newspapers,
# messy scans). Requires Ollama or any OpenAI-compatible endpoint at runtime.
cargo build --release --features vision-llm

# All three OCR backends at once
cargo build --release --features "ocr,ocrs,vision-llm"

# Install to your PATH
cargo install --path .
```

### Prerequisites

- **Rust 1.82+**: Install from [rustup.rs](https://rustup.rs)
- **Tesseract** (optional, for OCR):
  - Ubuntu/Debian: `sudo apt install tesseract-ocr libtesseract-dev libleptonica-dev`
  - Fedora: `sudo dnf install tesseract tesseract-devel leptonica-devel`
  - Windows: Download from [UB-Mannheim/tesseract](https://github.com/UB-Mannheim/tesseract/wiki)
  - macOS: `brew install tesseract`

## Usage

### Interactive TUI (recommended for first-time users)

Run `argus` with no arguments and you land in a full-screen dashboard:

```bash
argus
```

What you get:

| Section        | What it does                                                                                                    |
|----------------|-----------------------------------------------------------------------------------------------------------------|
| **search for** | the text/regex you're looking for                                                                               |
| **in folder**  | where to search (defaults to the directory you launched from, `~/` and `$HOME` are expanded)                    |
| **file types** | a row of toggleable chips for 22 common extensions — pick as many as you want, leave all unticked to scan everything |
| **options**    | case-sensitive, regex, OCR, include hidden, preview matches, **use saved index**, **save/update index**         |
| **OCR engine** | switch between Tesseract and `ocrs` live (only meaningful when OCR is enabled)                                  |
| **max depth**  | cycle through `unlimited / 1 / 2 / 3 / 5 / 10 / 20` levels                                                      |
| **limit**      | slider for the top-N cap on displayed matches                                                                   |
| **▶ run**      | runs the search                                                                                                 |

Key bindings:

- `Tab` / `Shift+Tab` — move between sections
- `Space` — toggle the highlighted chip / option (or flip the OCR engine)
- `←` / `→` — adjust sliders and pickers (extensions, OCR engine, max depth, limit)
- `Enter` — run the search from any field
- `Esc` — quit from Setup, go back from Results
- `↑` / `↓` + `Enter` — navigate the results list and open a file
- `n` — new search (keeps your filters, clears the query)
- `b` — back to Setup with everything preserved so you can tweak and re-run
- `Ctrl+C` — quit from anywhere

The Searching phase shows a live `current / total files` progress bar, so you can see exactly how the scan is going.

### Command-line usage

```bash
# Basic search in current directory
argus "search term"

# Search in a specific directory
argus -d /path/to/project "function"

# Case-sensitive search
argus -s "TODO"

# Use regex pattern
argus -r "\bfn\s+\w+"

# Search only specific file types
argus -e pdf,docx,txt "report"

# Enable OCR for images (requires --features ocr or --features ocrs)
argus -o "text in screenshot"

# Use the ocrs ONNX engine instead of Tesseract
argus -o --ocr-engine ocrs "text in screenshot"

# Use the vision-LLM backend (Ollama + glm-ocr) for handwriting / newspapers
# Requires: `ollama pull glm-ocr` and `cargo build --features vision-llm`
argus -o --ocr-engine vision-llm "handwritten shopping list"

# Use a cloud vision API instead of Ollama
ARGUS_OCR_API_KEY=sk-... argus -o --ocr-engine vision-llm \
    --ocr-endpoint https://api.openai.com/v1/chat/completions \
    --ocr-model gpt-4o-mini "receipt total"

# Show content preview
argus -p "error"

# Limit results
argus -l 50 "warning"

# Include hidden files
argus -H ".env"

# Set maximum directory depth
argus --max-depth 3 "config"

# Non-interactive mode (just print results)
argus -n "TODO"

# Save index for faster future searches
argus -i "pattern"

# Use existing index (skip re-extraction for unchanged files)
argus -I "pattern"

# Save and use index together (recommended for repeated searches)
argus -iI "pattern"

# Use a custom index file location
argus -i --index-file ~/my_index.json "pattern"
```

## Vision-LLM OCR (for handwriting, newspapers, messy scans)

Tesseract and ocrs are great on clean printed text, but they both fall off a cliff on **handwritten notes**, **newspapers with multi-column or rotated layouts**, and **skewed/misaligned scans**. The `vision-llm` backend delegates OCR to a vision model over HTTP and handles all three of those cases far better.

**Quick start with Ollama (local, offline, free)**:

```bash
# 1. Install Ollama — one-line installer on Linux / macOS, MSI on Windows.
#    https://ollama.com/download

# 2. Pull the default model (~1 GB, specialised for document OCR)
ollama pull glm-ocr

# 3. Build argus with the feature (Ollama doesn't need to be running yet)
cargo build --release --features vision-llm

# 4. Run — argus health-checks the endpoint up front, so a mis-typed URL
#    fails fast instead of piling up per-image timeouts.
argus -o --ocr-engine vision-llm "handwritten word"
```

**Cloud (OpenAI, Anthropic-compat, Mistral, Groq, Together, LM Studio)**:

```bash
export ARGUS_OCR_API_KEY=sk-...
argus -o --ocr-engine vision-llm \
    --ocr-endpoint https://api.openai.com/v1/chat/completions \
    --ocr-model gpt-4o-mini \
    "invoice total"
```

The API key is only ever read from the `ARGUS_OCR_API_KEY` environment variable, never from the CLI, so it doesn't leak into shell history or `ps` output.

**Recommended local models** (install with `ollama pull <name>`):

| Model | Size | Best at | Notes |
|---|---|---|---|
| **`glm-ocr`** *(default)* | ~1 GB | handwriting, newspapers, tables, math, multilingual docs | #1 on OmniDocBench V1.5, purpose-built for OCR, ~1.86 pages/s |
| `qwen2.5vl:7b` | ~5 GB | general multimodal + strong OCR | larger and slower, better at scene text and reasoning |
| `minicpm-v:8b` | ~5 GB | multilingual, CJK | good fallback on mixed-language pages |
| `llama3.2-vision:11b` | ~7.9 GB | general purpose | slightly over the 5 GB budget |
| `moondream:1.8b` | ~1.5 GB | low-end hardware | weaker but fast |

You can point `--ocr-model` at anything Ollama can serve; argus only cares that the endpoint speaks OpenAI-compatible chat completions.

## Command Line Options

| Flag | Long | Description | Default |
|------|------|-------------|---------|
| `[PATTERN]` | | Search pattern — **omit to launch the interactive TUI** | - |
| `-d` | `--directory` | Directory to search | Current dir |
| `-l` | `--limit` | Maximum results | 20 |
| `-s` | `--case-sensitive` | Case-sensitive search | Off |
| `-o` | `--ocr` | Enable OCR for images | Off |
| | `--ocr-engine` | Which OCR backend (`tesseract` \| `ocrs` \| `vision-llm`) | Depends on enabled feature |
| | `--ocr-endpoint` | Vision-LLM chat-completions URL | `http://localhost:11434/v1/chat/completions` |
| | `--ocr-model` | Vision-LLM model name | `glm-ocr` |
| | `--ocr-prompt` | Instruction passed to the vision LLM | "Extract all visible text…" |
| | `--ocr-timeout` | Vision-LLM per-request timeout (seconds) | 120 |
| `-r` | `--regex` | Use regex matching | Off |
| `-p` | `--preview` | Show match previews | Off |
| `-e` | `--extensions` | Filter by extensions (comma-separated) | All |
| | `--max-depth` | Max directory depth | Unlimited |
| `-H` | `--hidden` | Include hidden files | Off |
| `-n` | `--non-interactive` | Non-interactive mode | Off |
| `-i` | `--save-index` | Save index after scanning | Off |
| `-I` | `--use-index` | Use existing index | Off |
| | `--index-file` | Custom index file path | `.argus_index.json` |

| Env var | Purpose |
|---|---|
| `ARGUS_OCR_API_KEY` | Bearer token for the vision-LLM endpoint (OpenAI / Anthropic-compat / etc.). Never read from argv. |
| `ARGUS_OCRS_MODELS_DIR` | Override the ocrs model cache directory. |

Every option above is also surfaced in the TUI (with the exception of `--index-file`, `--ocr-endpoint`, `--ocr-model`, `--ocr-prompt`, and `--ocr-timeout`, which stay CLI-only to keep the Setup view compact — the TUI uses whatever values were in effect when argus was launched).

## Output Example

```
╔══════════════════════════════════════════════════════════════════╗
║  ARGUS - The All-Seeing Search Tool                              ║
╚══════════════════════════════════════════════════════════════════╝

  Stats: 1,234 files scanned, 42 matches in 8 files • 1.23s
  Types: PDF: 3 • Code: 4 • Text: 1

  Found 8 files with matches:

  #1  README.md • 12 matches [████████████ 100%]
      .../project/README.md
      "TODO: implement feature..."

  #2  src/main.rs • 8 matches [██████████░░ 83%]
      .../project/src/main.rs

  #3  docs/guide.pdf • 5 matches [████████░░░░ 67%]
      .../project/docs/guide.pdf
```

## Supported File Types

| Category | Extensions |
|----------|------------|
| **Text** | txt, md, markdown, rst, log, csv, json, yaml, yml, toml, xml, html |
| **Code** | rs, py, js, ts, jsx, tsx, java, c, cpp, go, rb, php, swift, and 40+ more |
| **Documents** | pdf, docx |
| **Images** (OCR) | png, jpg, jpeg, gif, bmp, tiff, webp |

## Build Scripts

### Linux/macOS

```bash
./build.sh
```

### Windows

```cmd
build.bat
```

## Architecture

```
src/
├── main.rs                 # CLI entry point; routes to the TUI or the classic CLI flow
├── types.rs                # Core data structures (SearchResult, Match, FileType, configs)
├── search.rs               # Parallel search engine with a shared progress handle
├── extractors.rs           # Text extraction for each file format; OCR dispatch
├── index.rs                # Index caching for extracted text
├── ocrs_backend.rs         # Pure-Rust ocrs ONNX OCR backend (feature = "ocrs")
├── vision_llm_backend.rs   # Vision-LLM OCR via OpenAI-compat HTTP (feature = "vision-llm")
├── ui.rs                   # Classic terminal output and interactive file selector
└── tui.rs                  # Full-screen ratatui dashboard (Setup → Searching → Results)
```

## Indexing

Argus can cache extracted text to an index file, making subsequent searches nearly instant. This is especially useful for:

- Large codebases or document collections
- Directories with PDFs, DOCX files, or images (expensive to extract)
- Repeated searches with different patterns

### How it works

1. **First run with `-i`**: Argus scans files, extracts text, and saves to `.argus_index.json`
2. **Subsequent runs with `-I`**: Argus loads the index and skips extraction for unchanged files
3. **Smart invalidation**: Modified files (different timestamp/size) are automatically re-extracted
4. **New files**: Automatically detected and added to the index

### Index file format

The index is stored as human-readable JSON:

```json
{
  "version": 1,
  "directory": "/path/to/searched/dir",
  "created_at": 1234567890,
  "updated_at": 1234567890,
  "entries": {
    "/path/to/file.txt": {
      "path": "/path/to/file.txt",
      "file_type": "Text",
      "extracted_text": "file contents...",
      "modified_timestamp": 1234567890,
      "file_size": 1234
    }
  }
}
```

## Performance Tips

1. **Use indexing** (`-iI`) for directories you search frequently
2. **Use extension filters** (`-e`) when you know the file types
3. **Set max depth** (`--max-depth`) for large directory trees
4. **Use literal search** instead of regex when possible
5. **OCR Performance**: When OCR is enabled, Argus uses thread-local Tesseract instances to avoid re-initialization overhead, enabling efficient parallel image processing across multiple CPU cores
6. **Faster OCR models**: Install `tesseract-langpack-eng-fast` (Fedora) or equivalent for ~2-3x faster OCR with slightly lower accuracy

## Troubleshooting

### OCR not working

**Tesseract** (`--features ocr`):
1. Ensure Tesseract is installed and in your PATH
2. Rebuild with: `cargo build --release --features ocr`
3. Check Tesseract works: `tesseract --version`

**ocrs** (`--features ocrs`):
1. First run downloads ~25 MB of models — ensure network connectivity, or pre-populate `ARGUS_OCRS_MODELS_DIR`
2. If a proxy / firewall blocks S3 downloads, set `ARGUS_OCRS_MODELS_DIR` to a directory containing `text-detection.rten` and `text-recognition.rten`

**Vision-LLM** (`--features vision-llm`):
1. `argus: vision-llm unavailable: vision-llm endpoint unreachable at …` — confirm `ollama serve` is running, and that `ollama list` shows the model you passed to `--ocr-model`. Pull it with e.g. `ollama pull glm-ocr`.
2. `argus: vision-llm HTTP 401: …` — for cloud endpoints, make sure `ARGUS_OCR_API_KEY` is exported in the same shell that runs argus.
3. Slow first call — the first request spins up the model in Ollama; subsequent calls are much faster.
4. Wrong model / 404 — Ollama model names often have a tag suffix (`qwen2.5vl:7b`, not `qwen2.5vl`). Check `ollama list`.

### Permission denied errors

Some files may be unreadable due to permissions. Argus will skip these and continue searching.

### Large files

Files over 50MB are automatically skipped to prevent memory issues.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

MIT License - see [LICENSE](LICENSE) for details.

## Acknowledgments

- Named after [Argus Panoptes](https://en.wikipedia.org/wiki/Argus_Panoptes), the hundred-eyed giant from Greek mythology
- Built with amazing Rust crates: clap, rayon, walkdir, colored, dialoguer, indicatif, ratatui, crossterm, and more
