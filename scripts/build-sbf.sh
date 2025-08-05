#!/bin/bash

# Default values
PROGRAM=""

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

# Run the cargo build command
echo "Building SBF program: $PROGRAM"
cargo build-sbf --tools-version v1.50 --manifest-path "$MANIFEST_PATH" --features bpf-entrypoint