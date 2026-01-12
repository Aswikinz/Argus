//! Text extraction from various file formats.

use crate::types::FileType;
use anyhow::{Context, Result};
use encoding_rs::UTF_8;
use encoding_rs_io::DecodeReaderBytesBuilder;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

/// Maximum file size to read (50 MB).
const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;

/// Result of text extraction.
#[derive(Debug)]
pub struct ExtractionResult {
    /// Extracted text content.
    pub text: String,
    /// Whether extraction was successful.
    pub success: bool,
    /// Error message if any.
    pub error: Option<String>,
}

impl ExtractionResult {
    /// Create a successful extraction result.
    pub fn success(text: String) -> Self {
        Self {
            text,
            success: true,
            error: None,
        }
    }

    /// Create a failed extraction result.
    pub fn failure(error: String) -> Self {
        Self {
            text: String::new(),
            success: false,
            error: Some(error),
        }
    }
}

/// Extract text from a file based on its type.
pub fn extract_text(path: &Path, file_type: FileType, ocr_enabled: bool) -> ExtractionResult {
    // Check file size first
    if let Ok(metadata) = path.metadata() {
        if metadata.len() > MAX_FILE_SIZE {
            return ExtractionResult::failure(format!(
                "File too large: {} bytes (max: {} bytes)",
                metadata.len(),
                MAX_FILE_SIZE
            ));
        }
    }

    match file_type {
        FileType::Text | FileType::Code | FileType::Other => extract_text_file(path),
        FileType::Pdf => extract_pdf(path),
        FileType::Docx => extract_docx(path),
        FileType::Image => {
            if ocr_enabled {
                extract_image_ocr(path)
            } else {
                ExtractionResult::failure("OCR not enabled for images".to_string())
            }
        }
    }
}

/// Extract text from a plain text file with encoding detection.
fn extract_text_file(path: &Path) -> ExtractionResult {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => return ExtractionResult::failure(format!("Failed to open file: {}", e)),
    };

    // Use encoding detection for text files
    let decoder = DecodeReaderBytesBuilder::new()
        .encoding(Some(UTF_8))
        .build(file);

    let reader = BufReader::new(decoder);
    let mut text = String::new();
    let mut line_count = 0;
    const MAX_LINES: usize = 100_000;

    for line_result in reader.lines() {
        match line_result {
            Ok(line) => {
                text.push_str(&line);
                text.push('\n');
                line_count += 1;
                if line_count >= MAX_LINES {
                    break;
                }
            }
            Err(e) => {
                // Try to continue on encoding errors
                if text.is_empty() {
                    return ExtractionResult::failure(format!("Failed to read file: {}", e));
                }
                break;
            }
        }
    }

    ExtractionResult::success(text)
}

/// Extract text from a PDF file.
fn extract_pdf(path: &Path) -> ExtractionResult {
    match pdf_extract::extract_text(path) {
        Ok(text) => {
            // Clean up the extracted text
            let cleaned = text
                .lines()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            ExtractionResult::success(cleaned)
        }
        Err(e) => ExtractionResult::failure(format!("Failed to extract PDF text: {}", e)),
    }
}

/// Extract text from a DOCX file.
fn extract_docx(path: &Path) -> ExtractionResult {
    match extract_docx_text(path) {
        Ok(text) => ExtractionResult::success(text),
        Err(e) => ExtractionResult::failure(format!("Failed to extract DOCX text: {}", e)),
    }
}

/// Internal DOCX text extraction using zip and xml parsing.
fn extract_docx_text(path: &Path) -> Result<String> {
    let file = File::open(path).context("Failed to open DOCX file")?;
    let mut archive = zip::ZipArchive::new(file).context("Failed to read DOCX as ZIP")?;

    // DOCX files store the main content in word/document.xml
    let mut document = archive
        .by_name("word/document.xml")
        .context("Failed to find document.xml in DOCX")?;

    let mut xml_content = String::new();
    document
        .read_to_string(&mut xml_content)
        .context("Failed to read document.xml")?;

    // Parse XML and extract text content
    let text = extract_text_from_docx_xml(&xml_content);
    Ok(text)
}

/// Extract text content from DOCX XML.
fn extract_text_from_docx_xml(xml: &str) -> String {
    let mut result = String::new();
    let mut in_text = false;
    let mut current_text = String::new();

    // Simple XML parsing to extract text between <w:t> tags
    let mut chars = xml.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '<' {
            // Start of tag
            let mut tag = String::new();
            while let Some(&next) = chars.peek() {
                if next == '>' {
                    chars.next();
                    break;
                }
                tag.push(chars.next().unwrap());
            }

            if tag.starts_with("w:t") && !tag.starts_with("w:t/") {
                in_text = true;
                current_text.clear();
            } else if tag == "/w:t" {
                in_text = false;
                result.push_str(&current_text);
            } else if tag == "/w:p" || tag.starts_with("/w:p ") {
                // End of paragraph - add newline
                result.push('\n');
            }
        } else if in_text {
            current_text.push(c);
        }
    }

    // Clean up multiple newlines
    let lines: Vec<&str> = result
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();

    lines.join("\n")
}

/// Extract text from an image using OCR (Tesseract).
/// Uses thread-local Tesseract instances for better performance with parallel processing.
#[cfg(feature = "ocr")]
fn extract_image_ocr(path: &Path) -> ExtractionResult {
    use leptess::LepTess;
    use std::cell::RefCell;

    // Thread-local Tesseract instance to avoid re-initialization overhead
    thread_local! {
        static TESSERACT: RefCell<Option<LepTess>> = RefCell::new(None);
    }

    TESSERACT.with(|cell| {
        let mut tess_opt = cell.borrow_mut();

        // Initialize Tesseract if not already done for this thread
        if tess_opt.is_none() {
            match LepTess::new(None, "eng") {
                Ok(lt) => *tess_opt = Some(lt),
                Err(e) => {
                    return ExtractionResult::failure(format!(
                        "Failed to initialize Tesseract: {}",
                        e
                    ))
                }
            }
        }

        let lt = tess_opt.as_mut().unwrap();

        // Set the image
        if let Err(e) = lt.set_image(path) {
            return ExtractionResult::failure(format!("Failed to load image for OCR: {}", e));
        }

        // Get text
        match lt.get_utf8_text() {
            Ok(text) => {
                let cleaned = text
                    .lines()
                    .map(|l| l.trim())
                    .filter(|l| !l.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                ExtractionResult::success(cleaned)
            }
            Err(e) => ExtractionResult::failure(format!("OCR extraction failed: {}", e)),
        }
    })
}

/// Stub for OCR when feature is disabled.
#[cfg(not(feature = "ocr"))]
fn extract_image_ocr(_path: &Path) -> ExtractionResult {
    ExtractionResult::failure(
        "OCR feature not enabled. Rebuild with --features ocr".to_string(),
    )
}

/// Check if a file is binary (non-text).
pub fn is_binary_file(path: &Path) -> bool {
    // Try to detect file type using magic bytes
    if let Ok(kind) = infer::get_from_path(path) {
        if let Some(k) = kind {
            let mime = k.mime_type();
            // Allow specific document types
            if mime == "application/pdf" || mime.starts_with("image/") {
                return false;
            }
            // Check if it's a known binary type
            if mime.starts_with("application/")
                && !mime.contains("json")
                && !mime.contains("xml")
                && !mime.contains("javascript")
            {
                return true;
            }
        }
    }

    // Fallback: read first bytes and check for null bytes
    if let Ok(mut file) = File::open(path) {
        let mut buffer = [0u8; 8192];
        if let Ok(n) = file.read(&mut buffer) {
            // Check for null bytes (common in binary files)
            let null_count = buffer[..n].iter().filter(|&&b| b == 0).count();
            if null_count > n / 10 {
                return true;
            }

            // Check for high proportion of non-printable characters
            let non_printable = buffer[..n]
                .iter()
                .filter(|&&b| b < 32 && b != b'\n' && b != b'\r' && b != b'\t')
                .count();
            if non_printable > n / 5 {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_docx_xml_extraction() {
        let xml = r#"<?xml version="1.0"?><w:document><w:body><w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t> World</w:t></w:r></w:p><w:p><w:r><w:t>Second paragraph</w:t></w:r></w:p></w:body></w:document>"#;
        let result = extract_text_from_docx_xml(xml);
        assert!(result.contains("Hello World"));
        assert!(result.contains("Second paragraph"));
    }

    #[test]
    fn test_file_type_detection() {
        assert_eq!(FileType::from_extension("pdf"), FileType::Pdf);
        assert_eq!(FileType::from_extension("docx"), FileType::Docx);
        assert_eq!(FileType::from_extension("rs"), FileType::Code);
        assert_eq!(FileType::from_extension("txt"), FileType::Text);
        assert_eq!(FileType::from_extension("png"), FileType::Image);
    }
}
