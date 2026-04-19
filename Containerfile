# Build variants (pass via --build-arg FEATURES=<value>):
#   ""      no OCR (default)   →  argus:latest
#   "ocr"   Tesseract OCR      →  argus:ocr
#   "ocrs"  pure-Rust ONNX OCR →  argus:ocrs  (models pre-baked into image)
#
# Usage:
#   podman build -t argus .
#   podman build -t argus:ocr  --build-arg FEATURES=ocr  .
#   podman build -t argus:ocrs --build-arg FEATURES=ocrs .
#
# Run:
#   podman run --rm -v "$(pwd):/data" argus /data "search term"
#   podman run -it --rm -v "$(pwd):/data" argus         # interactive TUI

ARG FEATURES=""

# ── Stage 1: builder ──────────────────────────────────────────────────────────
FROM rust:1.82-bookworm AS builder

ARG FEATURES

# Install build-time system deps.
# pkg-config  : required by many -sys crates to locate headers.
# libtesseract-dev / libleptonica-dev : C headers for leptonica-sys/tesseract-sys.
# libclang-dev / clang : bindgen (used by those -sys crates) needs libclang at
#   build time to generate FFI bindings.
# Installed unconditionally — cheap, avoids complex conditional shell logic,
# and future feature additions won't require Containerfile changes.
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libtesseract-dev \
        libleptonica-dev \
        libclang-dev \
        clang \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# ── Dependency cache layer ────────────────────────────────────────────────────
# Copy manifests first and build a stub binary so all ~80 transitive crates are
# compiled and cached in this layer. Source changes only rebuild argus itself.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs \
    && if [ -n "$FEATURES" ]; then \
           cargo build --release --locked --features "$FEATURES"; \
       else \
           cargo build --release --locked; \
       fi \
    && rm -rf src

# ── Application build ─────────────────────────────────────────────────────────
COPY src ./src
# Touch main.rs so Cargo detects the source change and relinks the real binary.
RUN touch src/main.rs \
    && if [ -n "$FEATURES" ]; then \
           cargo build --release --locked --features "$FEATURES"; \
       else \
           cargo build --release --locked; \
       fi

# ── Stage 2: model-fetcher ────────────────────────────────────────────────────
# Pre-downloads the ocrs ONNX models (~12 MB total) so the runtime container
# never needs outbound network access. When FEATURES != "ocrs" this stage just
# creates an empty directory — the COPY in the runtime stage is always valid.
FROM debian:bookworm-slim AS model-fetcher

ARG FEATURES

RUN apt-get update && apt-get install -y --no-install-recommends \
        curl \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /opt/argus/ocrs-models \
    && if [ "$FEATURES" = "ocrs" ]; then \
           curl -fsSL -o /opt/argus/ocrs-models/text-detection.rten \
               https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.rten \
           && curl -fsSL -o /opt/argus/ocrs-models/text-recognition.rten \
               https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.rten; \
       fi

# ── Stage 3: runtime ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

ARG FEATURES

# Base runtime libs always needed:
#   ca-certificates : TLS root certs (future-proofing / ureq).
#   libgcc-s1       : Rust panic unwinding runtime.
#   libssl3         : ureq/TLS is compiled into the ocrs binary even when the
#                     download path is never hit at runtime — dynamic linker
#                     requires it to be present.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
           ca-certificates \
           libgcc-s1 \
           libssl3 \
    && if [ "$FEATURES" = "ocr" ]; then \
           apt-get install -y --no-install-recommends \
               libtesseract5 \
               tesseract-ocr \
               tesseract-ocr-eng \
               libleptonica6; \
       fi \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/argus /usr/local/bin/argus

# Copy pre-baked ocrs models (empty directory for non-ocrs builds — harmless).
COPY --from=model-fetcher /opt/argus/ocrs-models /opt/argus/ocrs-models

# Tell the ocrs backend where to find the models. resolve_model_paths() checks
# this env var first (src/ocrs_backend.rs) and returns early without any
# network access when both model files are present.
ENV ARGUS_OCRS_MODELS_DIR=/opt/argus/ocrs-models

# Mount the directory to search at /data:
#   podman run --rm -v "$(pwd):/data" argus /data "pattern"
WORKDIR /data

ENTRYPOINT ["/usr/local/bin/argus"]
