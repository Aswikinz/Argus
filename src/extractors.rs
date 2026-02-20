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
        FileType::Pdf => extract_pdf(path, ocr_enabled),
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
/// When `ocr_enabled` is true, falls back to OCR on embedded images if text extraction
/// yields very little content (indicating a scanned/image-based PDF).
fn extract_pdf(path: &Path, ocr_enabled: bool) -> ExtractionResult {
    // First try normal text extraction
    let text_result = pdf_extract::extract_text(path);

    let cleaned = match text_result {
        Ok(text) => {
            text.lines()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        }
        Err(_) => String::new(),
    };

    // If we got substantial text, return it
    // A scanned PDF typically yields < 100 chars of garbage from pdf-extract
    let has_substantial_text = cleaned.len() > 100;

    if has_substantial_text || !ocr_enabled {
        if cleaned.is_empty() {
            return ExtractionResult::failure("Failed to extract PDF text".to_string());
        }
        return ExtractionResult::success(cleaned);
    }

    // OCR fallback: try extracting text from embedded images in the PDF
    #[cfg(feature = "ocr")]
    {
        let ocr_result = extract_pdf_images_ocr(path);
        if ocr_result.success && !ocr_result.text.is_empty() {
            // Combine any sparse text with OCR text
            if cleaned.is_empty() {
                return ocr_result;
            }
            let combined = format!("{}\n{}", cleaned, ocr_result.text);
            return ExtractionResult::success(combined);
        }

        // If OCR also failed but we have some text, return what we have
        if !cleaned.is_empty() {
            return ExtractionResult::success(cleaned);
        }
        return ExtractionResult::failure(
            "PDF appears to be scanned but OCR could not extract text".to_string(),
        );
    }

    #[cfg(not(feature = "ocr"))]
    {
        if cleaned.is_empty() {
            ExtractionResult::failure(
                "PDF appears to be scanned. Rebuild with --features ocr for OCR support"
                    .to_string(),
            )
        } else {
            ExtractionResult::success(cleaned)
        }
    }
}

/// Extract text from embedded images in a PDF using OCR.
/// This handles scanned PDFs where pages are stored as images.
#[cfg(feature = "ocr")]
fn extract_pdf_images_ocr(path: &Path) -> ExtractionResult {
    use lopdf::{Document, Object};

    let doc = match Document::load(path) {
        Ok(d) => d,
        Err(e) => {
            return ExtractionResult::failure(format!("Failed to parse PDF for OCR: {}", e))
        }
    };

    let mut all_text: Vec<String> = Vec::new();
    let mut image_count = 0;

    // Iterate through all objects looking for image streams
    for (_object_id, object) in &doc.objects {
        let stream = match object {
            Object::Stream(ref s) => s,
            _ => continue,
        };

        // Check if this is an Image XObject
        let is_image = stream
            .dict
            .get(b"Subtype")
            .map(|s| matches!(s, Object::Name(ref n) if n == b"Image"))
            .unwrap_or(false);

        if !is_image {
            continue;
        }

        // Get image dimensions
        let width = match stream.dict.get(b"Width") {
            Ok(Object::Integer(w)) => *w as u32,
            _ => continue,
        };
        let height = match stream.dict.get(b"Height") {
            Ok(Object::Integer(h)) => *h as u32,
            _ => continue,
        };

        // Skip very small images (icons, thumbnails, etc.)
        if width < 100 || height < 100 {
            continue;
        }

        // Determine the image filter (compression type)
        let filters = get_stream_filters(&stream.dict);

        // Try to extract and OCR this image
        if let Some(temp_file) = extract_image_from_pdf_stream(stream, &filters, width, height) {
            let ocr_result = extract_image_ocr(temp_file.path());
            if ocr_result.success && !ocr_result.text.trim().is_empty() {
                all_text.push(ocr_result.text);
                image_count += 1;
            }
        }
    }

    if all_text.is_empty() {
        ExtractionResult::failure(format!(
            "No readable text found in {} PDF image(s)",
            image_count
        ))
    } else {
        ExtractionResult::success(all_text.join("\n\n"))
    }
}

/// Get the list of filters applied to a PDF stream.
#[cfg(feature = "ocr")]
fn get_stream_filters(dict: &lopdf::Dictionary) -> Vec<Vec<u8>> {
    use lopdf::Object;

    match dict.get(b"Filter") {
        Ok(Object::Name(n)) => vec![n.clone()],
        Ok(Object::Array(arr)) => arr
            .iter()
            .filter_map(|o| {
                if let Object::Name(n) = o {
                    Some(n.clone())
                } else {
                    None
                }
            })
            .collect(),
        _ => vec![],
    }
}

/// Extract an image from a PDF stream and save to a temporary file.
/// Returns None if the image format is unsupported or extraction fails.
#[cfg(feature = "ocr")]
fn extract_image_from_pdf_stream(
    stream: &lopdf::Stream,
    filters: &[Vec<u8>],
    width: u32,
    height: u32,
) -> Option<tempfile::NamedTempFile> {
    use image::{DynamicImage, GrayImage, RgbImage};
    use lopdf::Object;
    use std::io::Write;

    let is_dct = filters.iter().any(|f| f == b"DCTDecode");
    let is_jpx = filters.iter().any(|f| f == b"JPXDecode");
    let is_flate = filters.iter().any(|f| f == b"FlateDecode");

    if is_dct {
        // DCTDecode = JPEG: the stream content is a valid JPEG file
        let mut temp = tempfile::Builder::new()
            .suffix(".jpg")
            .tempfile()
            .ok()?;
        temp.write_all(&stream.content).ok()?;
        temp.flush().ok()?;
        Some(temp)
    } else if is_jpx {
        // JPXDecode = JPEG2000: save the raw stream as .jp2
        let mut temp = tempfile::Builder::new()
            .suffix(".jp2")
            .tempfile()
            .ok()?;
        temp.write_all(&stream.content).ok()?;
        temp.flush().ok()?;
        Some(temp)
    } else if is_flate || filters.is_empty() {
        // FlateDecode or uncompressed: raw pixel data that needs reconstruction
        let mut stream_clone = stream.clone();
        stream_clone.decompress();
        let raw_data = stream_clone.content;

        // Determine color depth
        let bpc = match stream.dict.get(b"BitsPerComponent") {
            Ok(Object::Integer(b)) => *b as u8,
            _ => 8,
        };

        if bpc != 8 {
            return None; // Only handle 8-bit images for now
        }

        // Determine color space (DeviceGray=1ch, DeviceRGB=3ch)
        let channels = get_color_channels(&stream.dict);
        let expected_size = (width as usize) * (height as usize) * (channels as usize);

        if raw_data.len() < expected_size {
            return None; // Data doesn't match expected dimensions
        }

        // Construct image from raw pixels
        let img = match channels {
            1 => {
                let gray = GrayImage::from_raw(width, height, raw_data)?;
                DynamicImage::ImageLuma8(gray)
            }
            3 => {
                let rgb = RgbImage::from_raw(width, height, raw_data)?;
                DynamicImage::ImageRgb8(rgb)
            }
            _ => return None,
        };

        let temp = tempfile::Builder::new()
            .suffix(".png")
            .tempfile()
            .ok()?;
        img.save(temp.path()).ok()?;
        Some(temp)
    } else {
        None // Unsupported filter (CCITT, JBIG2, etc.)
    }
}

/// Determine the number of color channels from a PDF image's ColorSpace.
#[cfg(feature = "ocr")]
fn get_color_channels(dict: &lopdf::Dictionary) -> u8 {
    use lopdf::Object;

    match dict.get(b"ColorSpace") {
        Ok(Object::Name(ref name)) => match name.as_slice() {
            b"DeviceGray" | b"CalGray" => 1,
            b"DeviceRGB" | b"CalRGB" => 3,
            b"DeviceCMYK" => 4,
            _ => 3, // Default to RGB
        },
        Ok(Object::Array(ref arr)) => {
            // Indexed or ICCBased color spaces are arrays like [/ICCBased ref]
            if let Some(Object::Name(ref name)) = arr.first() {
                match name.as_slice() {
                    b"ICCBased" => 3, // Most common ICC profiles are RGB
                    b"Indexed" => 1,  // Palette-based
                    b"CalGray" => 1,
                    b"CalRGB" => 3,
                    _ => 3,
                }
            } else {
                3
            }
        }
        _ => 3, // Default to RGB if ColorSpace is missing or a reference
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
    if let Ok(Some(k)) = infer::get_from_path(path) {
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
