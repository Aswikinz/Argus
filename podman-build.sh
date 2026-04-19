#!/usr/bin/env bash
# Build the Argus container image and optionally extract the compiled binary
# to ./dist/ for native use on Linux.
#
# Usage:
#   ./podman-build.sh                          # no OCR  →  argus:latest
#   ./podman-build.sh --features ocr           # Tesseract  →  argus:ocr
#   ./podman-build.sh --features ocrs          # ONNX  →  argus:ocrs
#   ./podman-build.sh --features ocr --extract # build + copy binary to ./dist/
#   ./podman-build.sh --arm64                  # build for linux/arm64
#
# The extracted binary is a Linux ELF (glibc). It runs natively on Linux.
# macOS and Windows users should use "podman run" or "podman-compose run".

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

echo -e "${CYAN}"
echo "     █████╗ ██████╗  ██████╗ ██╗   ██╗███████╗"
echo "    ██╔══██╗██╔══██╗██╔════╝ ██║   ██║██╔════╝"
echo "    ███████║██████╔╝██║  ███╗██║   ██║███████╗"
echo "    ██╔══██║██╔══██╗██║   ██║██║   ██║╚════██║"
echo "    ██║  ██║██║  ██║╚██████╔╝╚██████╔╝███████║"
echo "    ╚═╝  ╚═╝╚═╝  ╚═╝ ╚═════╝  ╚═════╝ ╚══════╝"
echo -e "${NC}"
echo -e "${YELLOW}Podman Build Script${NC}"
echo ""

FEATURES=""
EXTRACT=false
PLATFORM="linux/amd64"
TAG_BASE="argus"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --features)       FEATURES="$2";   shift 2 ;;
        --features=*)     FEATURES="${1#--features=}"; shift ;;
        --extract)        EXTRACT=true;    shift ;;
        --arm64)          PLATFORM="linux/arm64"; shift ;;
        --platform)       PLATFORM="$2";   shift 2 ;;
        --platform=*)     PLATFORM="${1#--platform=}"; shift ;;
        --tag)            TAG_BASE="$2";   shift 2 ;;
        --tag=*)          TAG_BASE="${1#--tag=}"; shift ;;
        --help|-h)
            echo "Usage: ./podman-build.sh [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --features <FEAT>   Build variant: '' (default), 'ocr', or 'ocrs'"
            echo "  --extract           Copy built binary to ./dist/argus"
            echo "  --arm64             Build for linux/arm64"
            echo "  --platform <PLAT>   Target platform (default: linux/amd64)"
            echo "  --tag <NAME>        Image base name (default: argus)"
            echo ""
            echo "Examples:"
            echo "  ./podman-build.sh"
            echo "  ./podman-build.sh --features ocr"
            echo "  ./podman-build.sh --features ocrs"
            echo "  ./podman-build.sh --features ocr --extract"
            exit 0
            ;;
        *) echo -e "${RED}Unknown argument: $1${NC}" >&2; exit 1 ;;
    esac
done

if ! command -v podman &>/dev/null; then
    echo -e "${RED}Error: podman not found.${NC}"
    echo "Install from https://podman.io/getting-started/installation"
    exit 1
fi
echo -e "${GREEN}Podman found:${NC} $(podman --version)"
echo ""

if [[ -z "$FEATURES" ]]; then
    IMAGE_TAG="${TAG_BASE}:latest"
else
    IMAGE_TAG="${TAG_BASE}:${FEATURES}"
fi

echo -e "${CYAN}Build configuration:${NC}"
echo "  Image    : ${IMAGE_TAG}"
echo "  Features : ${FEATURES:-<none>}"
echo "  Platform : ${PLATFORM}"
echo "  Extract  : ${EXTRACT}"
echo ""

echo -e "${CYAN}Building image...${NC}"
echo ""

podman build \
    --file Containerfile \
    --tag "$IMAGE_TAG" \
    --platform "$PLATFORM" \
    --build-arg "FEATURES=${FEATURES}" \
    .

echo ""
echo -e "${GREEN}Build complete:${NC} ${IMAGE_TAG}"

IMAGE_SIZE=$(podman image inspect "$IMAGE_TAG" \
    --format '{{.Size}}' 2>/dev/null | \
    awk '{printf "%.1f MB", $1/1048576}')
echo -e "Image size: ${CYAN}${IMAGE_SIZE}${NC}"
echo ""

if [[ "$EXTRACT" == "true" ]]; then
    echo -e "${CYAN}Extracting binary to ./dist/ ...${NC}"
    mkdir -p ./dist

    # Create a stopped container, copy the binary out, remove it immediately.
    CNAME="argus-extract-$$"
    podman create --name "$CNAME" "$IMAGE_TAG" >/dev/null
    podman cp "${CNAME}:/usr/local/bin/argus" ./dist/argus
    podman rm "$CNAME" >/dev/null

    chmod +x ./dist/argus
    SIZE=$(du -h ./dist/argus | cut -f1)
    echo -e "${GREEN}Extracted:${NC} ./dist/argus  (${SIZE})"
    echo ""
    echo -e "${YELLOW}Note:${NC} This is a Linux ELF binary."
    echo "  Linux  :  ./dist/argus \"pattern\""
    echo "  WSL2   :  wsl ./dist/argus \"pattern\""
    echo ""
fi

echo -e "${YELLOW}Run in container:${NC}"
echo ""
echo "  # Non-interactive search:"
echo -e "  ${CYAN}podman run --rm -v \"\$(pwd):/data\" ${IMAGE_TAG} /data \"pattern\"${NC}"
echo ""
echo "  # Interactive TUI:"
echo -e "  ${CYAN}podman run -it --rm -v \"\$(pwd):/data\" ${IMAGE_TAG}${NC}"
echo ""
echo -e "${GREEN}Done!${NC}"
