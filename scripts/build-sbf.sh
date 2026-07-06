#!/bin/bash
set -euo pipefail

# Default values
PROGRAM=""
TOOLS_VERSION="v1.50"

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --program)
            PROGRAM="$2"
            shift 2
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 --program <program_name>"
            exit 1
            ;;
    esac
done

# Check if program parameter was provided
if [ -z "$PROGRAM" ]; then
    echo "Error: --program parameter is required"
    echo "Usage: $0 --program <program_name>"
    exit 1
fi

# Check if the Cargo.toml exists
MANIFEST_PATH="./programs/${PROGRAM}/Cargo.toml"
if [ ! -f "$MANIFEST_PATH" ]; then
    echo "Error: Cargo.toml not found at $MANIFEST_PATH"
    echo "Make sure the program '$PROGRAM' exists in ./programs/"
    exit 1
fi

PLATFORM_TOOLS="$HOME/.cache/solana/${TOOLS_VERSION}/platform-tools"
if [ ! -d "$PLATFORM_TOOLS" ]; then
    echo "Error: platform-tools ${TOOLS_VERSION} not found at $PLATFORM_TOOLS"
    echo "Run 'cargo build-sbf --tools-version ${TOOLS_VERSION}' once to install it"
    exit 1
fi

echo "Building SBF program: $PROGRAM (DWARF 5, unstripped)"
RUSTC_BOOTSTRAP=1 \
RUSTC="$PLATFORM_TOOLS/rust/bin/rustc" \
RUSTFLAGS="-C debuginfo=2 -C strip=none -Z dwarf-version=5" \
    "$PLATFORM_TOOLS/rust/bin/cargo" build \
    --release \
    --target sbpf-solana-solana \
    --manifest-path "$MANIFEST_PATH" \
    --features bpf-entrypoint

SO_NAME="${PROGRAM//-/_}"
TARGET_DIR="./programs/${PROGRAM}/target"
BUILT_SO="$TARGET_DIR/sbpf-solana-solana/release/${SO_NAME}.so"
DEPLOY_DIR="$TARGET_DIR/deploy"
OBJCOPY="$PLATFORM_TOOLS/llvm/bin/llvm-objcopy"
mkdir -p "$DEPLOY_DIR"
"$OBJCOPY" --strip-all "$BUILT_SO" "$DEPLOY_DIR/${SO_NAME}.so"
"$OBJCOPY" --only-keep-debug "$BUILT_SO" "$DEPLOY_DIR/${SO_NAME}.debug"
echo "Wrote $DEPLOY_DIR/${SO_NAME}.so and $DEPLOY_DIR/${SO_NAME}.debug"
