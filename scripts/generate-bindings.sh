#!/bin/bash
set -e

# Directory where the script is located
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ROOT_DIR="$SCRIPT_DIR/.."
BINDINGS_DIR="$ROOT_DIR/lattice-bindings"
OUTPUT_DIR="$ROOT_DIR/generated-bindings"

echo "Building lattice-bindings library..."
cargo build -p lattice-bindings

echo "Generating Swift bindings..."
mkdir -p "$OUTPUT_DIR/swift"
cargo run -p lattice-bindings --bin uniffi-bindgen generate \
    --library "$ROOT_DIR/target/debug/liblattice_bindings.dylib" \
    --language swift \
    --out-dir "$OUTPUT_DIR/swift"

echo "Generating Kotlin bindings..."
mkdir -p "$OUTPUT_DIR/kotlin"
cargo run -p lattice-bindings --bin uniffi-bindgen generate \
    --library "$ROOT_DIR/target/debug/liblattice_bindings.dylib" \
    --language kotlin \
    --out-dir "$OUTPUT_DIR/kotlin"

echo "Bindings generated in $OUTPUT_DIR"
