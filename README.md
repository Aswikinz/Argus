# Argus - The All-Seeing File Search Tool

Named after Argus Panoptes, the all-seeing giant from Greek mythology, **Argus** is a powerful CLI tool that searches for text across any file format.

## Features

- **Universal File Search**: Search through PDFs, Word documents (.docx), images (with OCR), text files, and code files
- **Fast Parallel Processing**: Leverages multi-core CPUs with Rayon for blazing-fast searches
- **Beautiful CLI**: Colorful output with file type icons, confidence bars, and match highlighting
- **Interactive Selection**: Navigate results with arrow keys and open files instantly
- **Regex Support**: Full regex pattern matching when you need precise searches
- **OCR Capability**: Extract and search text from images using Tesseract with optimized parallel processing (optional feature)
- **Cross-Platform**: Works on Linux and Windows

## Installation

### From Source

```bash
# Clone the repository
git clone https://github.com/Aswikinz/Argus.git
cd argus

# Build without OCR (faster build, smaller binary)
cargo build --release

# Build with OCR support (requires Tesseract installed)
cargo build --release --features ocr

# Install to your PATH
cargo install --path .
```

### Prerequisites

- **Rust 1.70+**: Install from [rustup.rs](https://rustup.rs)
- **Tesseract** (optional, for OCR):
  - Ubuntu/Debian: `sudo apt install tesseract-ocr libtesseract-dev libleptonica-dev`
  - Fedora: `sudo dnf install tesseract tesseract-devel leptonica-devel`
  - Windows: Download from [UB-Mannheim/tesseract](https://github.com/UB-Mannheim/tesseract/wiki)
  - macOS: `brew install tesseract`

## Usage

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

# Enable OCR for images (requires --features ocr)
argus -o "text in screenshot"

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
```

## Command Line Options

| Flag | Long | Description | Default |
|------|------|-------------|---------|
| `<PATTERN>` | | Search pattern (required) | - |
| `-d` | `--directory` | Directory to search | Current dir |
| `-l` | `--limit` | Maximum results | 20 |
| `-s` | `--case-sensitive` | Case-sensitive search | Off |
| `-o` | `--ocr` | Enable OCR for images | Off |
| `-r` | `--regex` | Use regex matching | Off |
| `-p` | `--preview` | Show match previews | Off |
| `-e` | `--extensions` | Filter by extensions | All |
| | `--max-depth` | Max directory depth | Unlimited |
| `-H` | `--hidden` | Include hidden files | Off |
| `-n` | `--non-interactive` | Non-interactive mode | Off |

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
├── main.rs        # CLI entry point and argument parsing
├── types.rs       # Core data structures (SearchResult, Match, FileType)
├── search.rs      # Search engine with parallel file processing
├── extractors.rs  # Text extraction for each file format
└── ui.rs          # Beautiful terminal output and interactive selection
```

## Performance Tips

1. **Use extension filters** (`-e`) when you know the file types
2. **Set max depth** (`--max-depth`) for large directory trees
3. **Use literal search** instead of regex when possible
4. **OCR Performance**: When OCR is enabled, Argus uses thread-local Tesseract instances to avoid re-initialization overhead, enabling efficient parallel image processing across multiple CPU cores
5. **Faster OCR models**: Install `tesseract-langpack-eng-fast` (Fedora) or equivalent for ~2-3x faster OCR with slightly lower accuracy

## Troubleshooting

### OCR not working

1. Ensure Tesseract is installed and in your PATH
2. Rebuild with: `cargo build --release --features ocr`
3. Check Tesseract works: `tesseract --version`

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
- Built with amazing Rust crates: clap, rayon, walkdir, colored, dialoguer, indicatif, and more
