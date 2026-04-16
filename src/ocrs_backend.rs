//! ocrs ONNX-based OCR backend.
//!
//! This backend uses the `ocrs` crate (pure-Rust, ONNX-based) for higher
//! accuracy than Tesseract on modern screenshots and documents. It loads two
//! `.rten` models from a per-user cache directory; if they're missing they
//! are downloaded on first use (~12 MB total, one-time).
//!
//! Users who prefer not to download at runtime can pre-populate the cache
//! directory or set `ARGUS_OCRS_MODELS_DIR` to point at a directory that
//! already contains `text-detection.rten` and `text-recognition.rten`.
//!
//! The engine is loaded once per process and shared across threads. If
//! initialization fails (e.g. network error on first download), the error is
//! cached and every subsequent call reports the same failure without retry.
//!
//! Prefer calling [`ensure_ready`] once at startup before entering any
//! parallel section — it triggers the one-time download synchronously so
//! rayon workers don't block on `OnceLock` while the models are being fetched.

use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use ocrs::{ImageSource, OcrEngine, OcrEngineParams};
use rten::Model;

const DETECTION_MODEL_URL: &str =
    "https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.rten";
const RECOGNITION_MODEL_URL: &str =
    "https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.rten";
const DETECTION_MODEL_FILE: &str = "text-detection.rten";
const RECOGNITION_MODEL_FILE: &str = "text-recognition.rten";
/// Upper bound for any single model file — guards against a malformed server
/// streaming garbage indefinitely.
const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;

/// Process-wide engine. Initialization failure is cached as a `String` so the
/// cell itself is `Sync`.
static ENGINE: OnceLock<std::result::Result<OcrEngine, String>> = OnceLock::new();

/// Eagerly initialize the engine (triggering any one-time model download).
///
/// Safe to call from a single thread before entering any parallel section.
/// Calling this up front avoids the situation where a rayon worker that is
/// the first to run OCR blocks every other worker on the `OnceLock` init
/// while it streams ~12 MB from the network.
pub fn ensure_ready() -> Result<()> {
    engine().map(|_| ())
}

/// Run OCR on an image file, returning the recognized text.
pub fn recognize(path: &Path) -> Result<String> {
    let engine = engine()?;

    let img = image::open(path)
        .with_context(|| format!("failed to open image for ocrs: {}", path.display()))?;
    let rgb = img.into_rgb8();
    let (w, h) = rgb.dimensions();
    let source = ImageSource::from_bytes(rgb.as_raw(), (w, h))
        .map_err(|e| anyhow!("invalid image for ocrs: {e:?}"))?;
    let input = engine.prepare_input(source)?;
    let text = engine.get_text(&input)?;
    Ok(text)
}

/// Return a reference to the shared engine, initializing it on first call.
fn engine() -> Result<&'static OcrEngine> {
    ENGINE
        .get_or_init(|| init_engine().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| anyhow!("ocrs engine unavailable: {e}"))
}

/// Load models and build the engine.
fn init_engine() -> Result<OcrEngine> {
    let (detection_path, recognition_path) = resolve_model_paths()?;

    let detection = Model::load_file(&detection_path).map_err(|e| {
        anyhow!(
            "failed to load detection model {}: {e}",
            detection_path.display()
        )
    })?;
    let recognition = Model::load_file(&recognition_path).map_err(|e| {
        anyhow!(
            "failed to load recognition model {}: {e}",
            recognition_path.display()
        )
    })?;

    OcrEngine::new(OcrEngineParams {
        detection_model: Some(detection),
        recognition_model: Some(recognition),
        ..Default::default()
    })
    .map_err(|e| anyhow!("failed to build ocrs engine: {e}"))
}

/// Return the paths to both model files, downloading them if necessary.
///
/// Resolution order:
/// 1. `ARGUS_OCRS_MODELS_DIR` environment variable (models must already exist
///    inside it).
/// 2. Per-user cache directory (downloads on first use).
fn resolve_model_paths() -> Result<(PathBuf, PathBuf)> {
    if let Ok(dir) = std::env::var("ARGUS_OCRS_MODELS_DIR") {
        let dir = PathBuf::from(dir);
        let det = dir.join(DETECTION_MODEL_FILE);
        let rec = dir.join(RECOGNITION_MODEL_FILE);
        if !det.exists() || !rec.exists() {
            return Err(anyhow!(
                "ARGUS_OCRS_MODELS_DIR={} does not contain both {} and {}",
                dir.display(),
                DETECTION_MODEL_FILE,
                RECOGNITION_MODEL_FILE
            ));
        }
        return Ok((det, rec));
    }

    let cache_dir = model_cache_dir()?;
    std::fs::create_dir_all(&cache_dir)
        .with_context(|| format!("failed to create cache directory {}", cache_dir.display()))?;

    let det = cache_dir.join(DETECTION_MODEL_FILE);
    let rec = cache_dir.join(RECOGNITION_MODEL_FILE);

    if !det.exists() {
        download_to_file(DETECTION_MODEL_URL, &det, "detection model")?;
    }
    if !rec.exists() {
        download_to_file(RECOGNITION_MODEL_URL, &rec, "recognition model")?;
    }

    Ok((det, rec))
}

/// Per-user cache directory for ocrs models.
fn model_cache_dir() -> Result<PathBuf> {
    let project_dirs = directories::ProjectDirs::from("com", "argus", "argus")
        .ok_or_else(|| anyhow!("could not determine per-user cache directory"))?;
    Ok(project_dirs.cache_dir().join("ocrs-models"))
}

/// Download a URL to a file with simple progress feedback on stderr.
///
/// Writes to `<dest>.part` and atomically renames to `dest` on success, so a
/// killed process can't leave a half-written model in place.
fn download_to_file(url: &str, dest: &Path, label: &str) -> Result<()> {
    let response = ureq::get(url)
        .call()
        .with_context(|| format!("failed to request {url}"))?;

    let total = response
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok());

    eprintln!(
        "  downloading ocrs {label}{}  →  {}",
        total
            .map(|b| format!(" ({:.1} MB)", b as f64 / 1_048_576.0))
            .unwrap_or_default(),
        dest.display(),
    );

    let tmp = dest.with_extension("rten.part");
    let mut file = std::fs::File::create(&tmp)
        .with_context(|| format!("failed to create {}", tmp.display()))?;

    let mut reader = response.into_reader().take(MAX_DOWNLOAD_BYTES);
    let mut buf = [0u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    let mut last_printed_pct: i32 = -1;
    let stderr_is_tty = is_stderr_tty();

    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("network read failed for {url}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .with_context(|| format!("failed to write to {}", tmp.display()))?;
        downloaded += n as u64;

        if let Some(total) = total {
            let pct = ((downloaded as f64 / total as f64) * 100.0).round() as i32;
            if pct != last_printed_pct {
                last_printed_pct = pct;
                if stderr_is_tty {
                    // Single repainted line so the user sees live progress.
                    eprint!(
                        "\r  downloading ocrs {label}  {pct:>3}%  {:.1}/{:.1} MB",
                        downloaded as f64 / 1_048_576.0,
                        total as f64 / 1_048_576.0,
                    );
                    let _ = std::io::stderr().flush();
                } else if pct % 10 == 0 {
                    // On non-TTY (CI, pipes) log at 10% intervals, newline-terminated.
                    eprintln!(
                        "  downloading ocrs {label}  {pct:>3}%  {:.1}/{:.1} MB",
                        downloaded as f64 / 1_048_576.0,
                        total as f64 / 1_048_576.0,
                    );
                }
            }
        }
    }

    if stderr_is_tty && total.is_some() {
        // Finish the in-place progress line with a newline.
        eprintln!();
    }

    std::fs::rename(&tmp, dest)
        .with_context(|| format!("failed to move model into place at {}", dest.display()))?;
    eprintln!(
        "  ready: {} ({:.1} MB)",
        label,
        downloaded as f64 / 1_048_576.0
    );
    Ok(())
}

/// Is stderr attached to a terminal that understands CR repainting?
fn is_stderr_tty() -> bool {
    std::io::stderr().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_cache_dir_returns_nonempty_path() {
        let path = model_cache_dir().expect("cache dir should resolve");
        assert!(!path.as_os_str().is_empty());
        // Path ends with our subdirectory for isolation.
        assert!(path.ends_with("ocrs-models"));
    }

    #[test]
    fn env_override_reports_missing_files() {
        // Pointing at an empty directory should fail loudly instead of
        // silently falling back to the cache path.
        let dir = tempfile::tempdir().unwrap();
        let prev = std::env::var("ARGUS_OCRS_MODELS_DIR").ok();
        // SAFETY: tests modifying env are single-threaded via `cargo test`
        // default behavior only when `--test-threads=1`. We restore below.
        std::env::set_var("ARGUS_OCRS_MODELS_DIR", dir.path());
        let result = resolve_model_paths();
        match prev {
            Some(v) => std::env::set_var("ARGUS_OCRS_MODELS_DIR", v),
            None => std::env::remove_var("ARGUS_OCRS_MODELS_DIR"),
        }
        let err = result.expect_err("should fail on empty override dir");
        let msg = format!("{err}");
        assert!(msg.contains("text-detection.rten") || msg.contains("text-recognition.rten"));
    }
}
