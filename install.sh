#!/bin/bash
set -e

# Shammah Installation Script
# Usage: curl -sSL https://raw.githubusercontent.com/schancel/shammah/main/install.sh | bash

VERSION="${VERSION:-latest}"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}╔═══════════════════════════════════════╗${NC}"
echo -e "${GREEN}║   Shammah Installation Script        ║${NC}"
echo -e "${GREEN}║   Local-First AI Coding Assistant     ║${NC}"
echo -e "${GREEN}╚═══════════════════════════════════════╝${NC}"
echo ""

# Detect OS and Architecture
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Darwin)
        OS_NAME="macos"
        case "$ARCH" in
            x86_64)
                PLATFORM="macos-x86_64"
                ;;
            arm64|aarch64)
                PLATFORM="macos-aarch64"
                ;;
            *)
                echo -e "${RED}✗ Unsupported architecture: $ARCH${NC}"
                exit 1
                ;;
        esac
        ;;
    Linux)
        OS_NAME="linux"
        case "$ARCH" in
            x86_64)
                PLATFORM="linux-x86_64"
                ;;
            *)
                echo -e "${RED}✗ Unsupported architecture: $ARCH${NC}"
                echo -e "${YELLOW}Try building from source: https://github.com/schancel/shammah#installation${NC}"
                exit 1
                ;;
        esac
        ;;
    *)
        echo -e "${RED}✗ Unsupported OS: $OS${NC}"
        echo -e "${YELLOW}Try building from source: https://github.com/schancel/shammah#installation${NC}"
        exit 1
        ;;
esac

echo -e "${GREEN}✓${NC} Detected platform: ${GREEN}$PLATFORM${NC}"

# Determine download URL
if [ "$VERSION" = "latest" ]; then
    DOWNLOAD_URL="https://github.com/schancel/shammah/releases/latest/download/shammah-$PLATFORM.tar.gz"
else
    DOWNLOAD_URL="https://github.com/schancel/shammah/releases/download/$VERSION/shammah-$PLATFORM.tar.gz"
fi

echo -e "${GREEN}✓${NC} Download URL: $DOWNLOAD_URL"

# Create install directory
mkdir -p "$INSTALL_DIR"
echo -e "${GREEN}✓${NC} Install directory: ${GREEN}$INSTALL_DIR${NC}"

# Download and extract
TEMP_DIR="$(mktemp -d)"
cd "$TEMP_DIR"

echo ""
echo -e "${YELLOW}⏳${NC} Downloading Shammah..."
if ! curl -sSL "$DOWNLOAD_URL" -o shammah.tar.gz; then
    echo -e "${RED}✗ Failed to download Shammah${NC}"
    echo -e "${YELLOW}Check if the release exists: https://github.com/schancel/shammah/releases${NC}"
    rm -rf "$TEMP_DIR"
    exit 1
fi

echo -e "${GREEN}✓${NC} Downloaded"

echo -e "${YELLOW}⏳${NC} Extracting..."
tar -xzf shammah.tar.gz

echo -e "${YELLOW}⏳${NC} Installing to $INSTALL_DIR..."
mv shammah "$INSTALL_DIR/shammah"
chmod +x "$INSTALL_DIR/shammah"

# Cleanup
cd - > /dev/null
rm -rf "$TEMP_DIR"

echo -e "${GREEN}✓${NC} Installed successfully!"
echo ""

# Check if install dir is in PATH
if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
    echo -e "${YELLOW}⚠️  Warning: $INSTALL_DIR is not in your PATH${NC}"
    echo ""
    echo "Add this to your shell profile (~/.bashrc, ~/.zshrc, etc.):"
    echo ""
    echo -e "${GREEN}export PATH=\"\$PATH:$INSTALL_DIR\"${NC}"
    echo ""
fi

# Test installation
echo -e "${GREEN}✓${NC} Testing installation..."
if "$INSTALL_DIR/shammah" --version > /dev/null 2>&1; then
    VERSION_OUTPUT="$($INSTALL_DIR/shammah --version)"
    echo -e "${GREEN}✓${NC} Shammah $VERSION_OUTPUT installed!"
else
    echo -e "${RED}✗ Installation test failed${NC}"
    exit 1
fi

echo ""
echo -e "${GREEN}╔═══════════════════════════════════════╗${NC}"
echo -e "${GREEN}║   Installation Complete! 🎉           ║${NC}"
echo -e "${GREEN}╚═══════════════════════════════════════╝${NC}"
echo ""
echo "Next steps:"
echo ""
echo -e "1. Run setup wizard:     ${GREEN}shammah setup${NC}"
echo -e "2. Start using it:       ${GREEN}shammah${NC}"
echo ""
echo "Documentation: https://github.com/schancel/shammah"
echo ""
