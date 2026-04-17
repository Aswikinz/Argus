//! Vision-LLM OCR backend.
//!
//! Sends a base64-encoded image to an OpenAI-compatible chat-completions
//! endpoint and returns the model's transcription. The goal is a single
//! HTTP-only backend that speaks to:
//!
//! * **Ollama** running any vision model locally (default: `glm-ocr`, a
//!   0.9 B parameter model purpose-built for document OCR, ~1 GB on disk).
//! * **OpenAI / Mistral / Groq / Together / LM Studio** — any service that
//!   exposes `/v1/chat/completions` with `image_url` content parts.
//! * **Anthropic** via an openai-compat proxy (it speaks the same payload
//!   shape through our parse path).
//!
//! Why this backend exists: Tesseract and `ocrs` plateau on three common
//! cases — handwritten notes, newspapers with multi-column / rotated text,
//! and skewed scans. A well-trained vision LLM handles all three without
//! additional preprocessing; GLM-OCR specifically is the current leader on
//! OmniDocBench V1.5 despite being ten times smaller than the generalists.
//!
//! The public surface mirrors [`crate::ocrs_backend`]: callers get
//! [`ensure_ready`] for an eager health-check and [`recognize`] for the
//! per-image call. Internal helpers ([`build_request`], [`parse_response`])
//! are kept small enough to unit-test without a live server.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde_json::{json, Value};

use crate::types::VisionLlmConfig;

/// Cached HTTP agent, reused across rayon workers.
///
/// The agent stores the connection pool (so repeated image uploads reuse the
/// TCP/TLS connection) and the configured timeout. It's created on first use
/// and keyed to the first config we see — in practice that's fine because
/// the config is immutable for the duration of a single `argus` invocation.
static AGENT: OnceLock<ureq::Agent> = OnceLock::new();

fn agent_for(cfg: &VisionLlmConfig) -> &'static ureq::Agent {
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .build()
    })
}

/// Minimum sanity-check on the config and a cheap HEAD/OPTIONS-style ping.
///
/// Called from `main.rs` before the search starts so the user sees a fast,
/// actionable error ("Ollama isn't running at ...") instead of 1 s-per-image
/// timeouts piling up once rayon is in the middle of a scan.
///
/// We don't require the upstream to support a specific health endpoint —
/// many don't. Instead we try a cheap GET against the *origin* of the
/// endpoint URL; any 2xx/3xx/4xx reply (even "405 Method Not Allowed") is
/// proof that the server is reachable. Only connection refusals and DNS
/// failures escape as errors.
pub fn ensure_ready(cfg: &VisionLlmConfig) -> Result<()> {
    if cfg.endpoint.trim().is_empty() {
        return Err(anyhow!("vision-llm endpoint is empty"));
    }
    if cfg.model.trim().is_empty() {
        return Err(anyhow!("vision-llm model name is empty"));
    }

    let origin = health_check_url(&cfg.endpoint);
    let agent = agent_for(cfg);

    match agent.get(&origin).call() {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(_code, _resp)) => {
            // Any HTTP response — even a 404 or 405 — means something is
            // listening. That's good enough for readiness.
            Ok(())
        }
        Err(e) => Err(anyhow!(
            "vision-llm endpoint unreachable at {}: {e}. \
             If you're using Ollama, check that `ollama serve` is running \
             and that the model is pulled (e.g. `ollama pull {}`).",
            origin,
            cfg.model,
        )),
    }
}

/// Run OCR on an image file, returning the recognized text.
pub fn recognize(path: &Path, cfg: &VisionLlmConfig) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read image for vision-llm: {}", path.display()))?;
    let mime = guess_mime(&bytes, path);
    let body = build_request(&bytes, mime, cfg);

    let agent = agent_for(cfg);
    let mut req = agent
        .post(&cfg.endpoint)
        .set("Content-Type", "application/json");
    if let Some(ref key) = cfg.api_key {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }

    let response = req.send_json(body).map_err(|e| match e {
        ureq::Error::Status(code, resp) => {
            // Drain the body for the error message — any extra context the
            // server provides is more useful than a bare status code.
            let detail = resp
                .into_string()
                .unwrap_or_else(|_| "<unreadable body>".to_string());
            anyhow!("vision-llm HTTP {code}: {}", truncate(&detail, 400))
        }
        other => anyhow!("vision-llm request failed: {other}"),
    })?;

    let json: Value = response
        .into_json()
        .context("vision-llm response was not valid JSON")?;

    parse_response(&json)
}

/// Build the OpenAI-compatible chat-completions payload for a single image.
///
/// Shape:
/// ```json
/// {
///   "model": "<cfg.model>",
///   "temperature": 0.0,
///   "messages": [{
///     "role": "user",
///     "content": [
///       { "type": "text", "text": "<cfg.prompt>" },
///       { "type": "image_url",
///         "image_url": { "url": "data:image/png;base64,..." } }
///     ]
///   }]
/// }
/// ```
///
/// Kept pure so tests can round-trip golden fixtures without the network.
pub(crate) fn build_request(image_bytes: &[u8], mime: &str, cfg: &VisionLlmConfig) -> Value {
    let encoded = BASE64.encode(image_bytes);
    let data_url = format!("data:{mime};base64,{encoded}");

    json!({
        "model": cfg.model,
        "temperature": 0.0,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": cfg.prompt },
                { "type": "image_url", "image_url": { "url": data_url } }
            ]
        }]
    })
}

/// Pull the assistant text out of an OpenAI-compatible response body.
///
/// Handles three common shapes:
/// * OpenAI / OpenRouter / Groq / Together — `choices[0].message.content` is
///   a string.
/// * Anthropic-via-openai-compat — `choices[0].message.content` is an array
///   of `{type, text}` parts. Concatenate the text parts.
/// * Ollama — same as OpenAI, so the first branch already matches.
///
/// Also surfaces API-level error payloads (`{"error": {...}}`) as Rust errors.
pub(crate) fn parse_response(json: &Value) -> Result<String> {
    if let Some(err) = json.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_else(|| err.as_str().unwrap_or("unknown error"));
        return Err(anyhow!("vision-llm API error: {msg}"));
    }

    let choice = json
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .ok_or_else(|| {
            anyhow!(
                "vision-llm response had no choices: {}",
                truncate_json(json)
            )
        })?;

    let content = choice
        .get("message")
        .and_then(|m| m.get("content"))
        .ok_or_else(|| anyhow!("vision-llm response had no message.content"))?;

    if let Some(s) = content.as_str() {
        return Ok(s.trim().to_string());
    }

    if let Some(parts) = content.as_array() {
        let mut out = String::new();
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        if !out.is_empty() {
            return Ok(out.trim().to_string());
        }
    }

    Err(anyhow!(
        "vision-llm response had unexpected content shape: {}",
        truncate_json(content)
    ))
}

/// Guess a MIME type from the image magic bytes, falling back to the path's
/// extension and finally to `image/png`.
fn guess_mime(bytes: &[u8], path: &Path) -> &'static str {
    if let Ok(format) = image::guess_format(bytes) {
        return match format {
            image::ImageFormat::Jpeg => "image/jpeg",
            image::ImageFormat::Gif => "image/gif",
            image::ImageFormat::WebP => "image/webp",
            image::ImageFormat::Tiff => "image/tiff",
            image::ImageFormat::Bmp => "image/bmp",
            // PNG and every other format we didn't list explicitly are sent
            // as image/png — it's a safe universal default that every OCR
            // VLM accepts.
            _ => "image/png",
        };
    }
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("tiff" | "tif") => "image/tiff",
        Some("bmp") => "image/bmp",
        _ => "image/png",
    }
}

/// Derive a best-effort "origin" URL for the readiness check — strip the
/// path so we hit the server's root rather than POSTing to
/// `/v1/chat/completions` with a GET. Falls back to the raw endpoint if
/// parsing fails.
fn health_check_url(endpoint: &str) -> String {
    if let Some(scheme_end) = endpoint.find("://") {
        let after_scheme = &endpoint[scheme_end + 3..];
        if let Some(slash) = after_scheme.find('/') {
            return endpoint[..scheme_end + 3 + slash].to_string();
        }
    }
    endpoint.to_string()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let kept: String = s.chars().take(max).collect();
        format!("{kept}…")
    }
}

fn truncate_json(v: &Value) -> String {
    truncate(&v.to_string(), 200)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> VisionLlmConfig {
        VisionLlmConfig {
            endpoint: "http://localhost:11434/v1/chat/completions".to_string(),
            model: "glm-ocr".to_string(),
            api_key: None,
            prompt: "Extract text.".to_string(),
            timeout_secs: 30,
        }
    }

    #[test]
    fn build_request_has_expected_top_level_shape() {
        let body = build_request(b"\x89PNG\r\n\x1a\n", "image/png", &cfg());
        assert_eq!(body["model"], "glm-ocr");
        assert_eq!(body["temperature"], 0.0);
        let msgs = body["messages"].as_array().expect("messages array");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
    }

    #[test]
    fn build_request_encodes_image_as_data_url() {
        let body = build_request(b"payload-bytes", "image/jpeg", &cfg());
        let url = body["messages"][0]["content"][1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(url.starts_with("data:image/jpeg;base64,"));
        // Base64 of "payload-bytes" = "cGF5bG9hZC1ieXRlcw=="
        assert!(url.ends_with("cGF5bG9hZC1ieXRlcw=="));
    }

    #[test]
    fn build_request_places_prompt_as_first_content_part() {
        let body = build_request(b"x", "image/png", &cfg());
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(body["messages"][0]["content"][0]["text"], "Extract text.");
    }

    #[test]
    fn parse_response_openai_shape() {
        let resp = json!({
            "choices": [{
                "message": { "role": "assistant", "content": "  hello world  " }
            }]
        });
        assert_eq!(parse_response(&resp).unwrap(), "hello world");
    }

    #[test]
    fn parse_response_anthropic_via_compat_shape() {
        // Some proxies (e.g. Anthropic-via-openai-compat) return an array of
        // content parts instead of a single string.
        let resp = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": [
                        { "type": "text", "text": "line one" },
                        { "type": "text", "text": "line two" }
                    ]
                }
            }]
        });
        assert_eq!(parse_response(&resp).unwrap(), "line one\nline two");
    }

    #[test]
    fn parse_response_surfaces_api_error_object() {
        let resp = json!({
            "error": { "message": "model not found", "type": "invalid_request" }
        });
        let err = parse_response(&resp).unwrap_err().to_string();
        assert!(err.contains("model not found"), "got: {err}");
    }

    #[test]
    fn parse_response_errors_when_choices_missing() {
        let resp = json!({});
        assert!(parse_response(&resp).is_err());
    }

    #[test]
    fn parse_response_errors_on_unexpected_content_shape() {
        let resp = json!({
            "choices": [{ "message": { "content": 42 } }]
        });
        assert!(parse_response(&resp).is_err());
    }

    #[test]
    fn parse_response_ollama_shape_matches_openai_shape() {
        // Ollama's openai-compat endpoint returns the exact OpenAI shape.
        let resp = json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 0,
            "model": "glm-ocr",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "transcribed" },
                "finish_reason": "stop"
            }]
        });
        assert_eq!(parse_response(&resp).unwrap(), "transcribed");
    }

    #[test]
    fn guess_mime_detects_png_magic_bytes() {
        let png_magic = b"\x89PNG\r\n\x1a\nrest";
        let mime = guess_mime(png_magic, Path::new("whatever.bin"));
        assert_eq!(mime, "image/png");
    }

    #[test]
    fn guess_mime_detects_jpeg_magic_bytes() {
        let jpeg_magic = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00";
        let mime = guess_mime(jpeg_magic, Path::new("whatever.bin"));
        assert_eq!(mime, "image/jpeg");
    }

    #[test]
    fn guess_mime_falls_back_to_extension() {
        // These bytes aren't recognizable as any image format; we rely on
        // the path extension.
        let unrecognizable = b"not an image";
        assert_eq!(
            guess_mime(unrecognizable, Path::new("scan.jpg")),
            "image/jpeg"
        );
        assert_eq!(
            guess_mime(unrecognizable, Path::new("scan.JPEG")),
            "image/jpeg"
        );
        assert_eq!(
            guess_mime(unrecognizable, Path::new("scan.bmp")),
            "image/bmp"
        );
    }

    #[test]
    fn guess_mime_final_fallback_is_png() {
        let unrecognizable = b"not an image";
        assert_eq!(
            guess_mime(unrecognizable, Path::new("scan.unknown")),
            "image/png"
        );
    }

    #[test]
    fn health_check_url_strips_path() {
        assert_eq!(
            health_check_url("http://localhost:11434/v1/chat/completions"),
            "http://localhost:11434"
        );
        assert_eq!(
            health_check_url("https://api.openai.com/v1/chat/completions"),
            "https://api.openai.com"
        );
    }

    #[test]
    fn health_check_url_tolerates_bare_host() {
        assert_eq!(
            health_check_url("http://localhost:11434"),
            "http://localhost:11434"
        );
        // Not a URL at all — we pass it through unchanged.
        assert_eq!(health_check_url("not-a-url"), "not-a-url");
    }

    #[test]
    fn ensure_ready_rejects_empty_endpoint() {
        let mut c = cfg();
        c.endpoint = "   ".to_string();
        assert!(ensure_ready(&c).is_err());
    }

    #[test]
    fn ensure_ready_rejects_empty_model() {
        let mut c = cfg();
        c.model = String::new();
        assert!(ensure_ready(&c).is_err());
    }

    #[test]
    fn truncate_returns_short_strings_unchanged() {
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn truncate_appends_ellipsis() {
        let out = truncate("abcdefghij", 5);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 6);
    }
}
