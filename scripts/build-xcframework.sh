#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
BUILD_DIR="$ROOT_DIR/target"
HEADERS_DIR="$ROOT_DIR/generated-bindings/swift"
INCLUDE_DIR="$ROOT_DIR/target/headers"
OUTPUT_DIR="$ROOT_DIR"

export MACOSX_DEPLOYMENT_TARGET=26.0


# Targets (Apple Silicon focused)
MAC_TARGET="aarch64-apple-darwin"
IOS_TARGET="aarch64-apple-ios"
SIM_TARGET="aarch64-apple-ios-sim"

echo "Checking Rust targets..."
rustup target add $MAC_TARGET $IOS_TARGET $SIM_TARGET

echo "Building lattice-bindings and lattice-api..."
echo "  - macOS ($MAC_TARGET)..."
cargo build -p lattice-bindings -p lattice-api --features lattice-api/ffi --release --target $MAC_TARGET
echo "  - iOS Device ($IOS_TARGET)..."
cargo build -p lattice-bindings -p lattice-api --features lattice-api/ffi --release --target $IOS_TARGET
echo "  - iOS Simulator ($SIM_TARGET)..."
cargo build -p lattice-bindings -p lattice-api --features lattice-api/ffi --release --target $SIM_TARGET

# Merge static libraries (lattice_bindings + lattice_api FFI symbols)
echo "Merging static libraries..."
MERGED_DIR="$BUILD_DIR/merged"
mkdir -p "$MERGED_DIR/$MAC_TARGET" "$MERGED_DIR/$IOS_TARGET" "$MERGED_DIR/$SIM_TARGET"

libtool -static -o "$MERGED_DIR/$MAC_TARGET/liblattice.a" \
    "$BUILD_DIR/$MAC_TARGET/release/liblattice_bindings.a" \
    "$BUILD_DIR/$MAC_TARGET/release/liblattice_api.a"

libtool -static -o "$MERGED_DIR/$IOS_TARGET/liblattice.a" \
    "$BUILD_DIR/$IOS_TARGET/release/liblattice_bindings.a" \
    "$BUILD_DIR/$IOS_TARGET/release/liblattice_api.a"

libtool -static -o "$MERGED_DIR/$SIM_TARGET/liblattice.a" \
    "$BUILD_DIR/$SIM_TARGET/release/liblattice_bindings.a" \
    "$BUILD_DIR/$SIM_TARGET/release/liblattice_api.a"

# Prepare Headers
echo "Preparing headers..."
mkdir -p "$INCLUDE_DIR"
# Copy both FFI headers
cp "$HEADERS_DIR/lattice_bindingsFFI.h" "$INCLUDE_DIR/"
cp "$HEADERS_DIR/lattice_apiFFI.h" "$INCLUDE_DIR/"
# Create combined module map
cat > "$INCLUDE_DIR/module.modulemap" <<EOF
module lattice_bindingsFFI {
    header "lattice_bindingsFFI.h"
    export *
}
module lattice_apiFFI {
    header "lattice_apiFFI.h"
    export *
}
EOF

# Create XCFramework
echo "Creating XCFramework..."
rm -rf "$OUTPUT_DIR/LatticeBindings.xcframework"

xcodebuild -create-xcframework \
    -library "$MERGED_DIR/$MAC_TARGET/liblattice.a" \
    -headers "$INCLUDE_DIR" \
    -library "$MERGED_DIR/$IOS_TARGET/liblattice.a" \
    -headers "$INCLUDE_DIR" \
    -library "$MERGED_DIR/$SIM_TARGET/liblattice.a" \
    -headers "$INCLUDE_DIR" \
    -output "$OUTPUT_DIR/LatticeBindings.xcframework"

echo "Created LatticeBindings.xcframework in $OUTPUT_DIR"
