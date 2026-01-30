#!/bin/bash
# setup-app.sh - Builds XCFramework and sets up LatticeApp

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
APP_DIR="$SCRIPT_DIR/LatticeApp"
GENERATED_DIR="$ROOT_DIR/generated-bindings/swift"

echo "Generating Swift bindings..."
"$ROOT_DIR/scripts/generate-bindings.sh"

echo "Building Universal XCFramework (iOS + macOS)..."
"$ROOT_DIR/scripts/build-xcframework.sh"

echo "Copying to app..."
# Copy XCFramework
rm -rf "$APP_DIR/LatticeBindings.xcframework"
cp -R "$ROOT_DIR/LatticeBindings.xcframework" "$APP_DIR/"

# Copy Swift bindings
mkdir -p "$APP_DIR/Bindings"
cp "$GENERATED_DIR/lattice_bindings.swift" "$APP_DIR/Bindings/"
cp "$GENERATED_DIR/lattice_api.swift" "$APP_DIR/Bindings/"

# Post-process Swift files to import both FFI modules
sed -i '' 's/import lattice_bindingsFFI/import lattice_bindingsFFI\nimport lattice_apiFFI/' "$APP_DIR/Bindings/lattice_bindings.swift"
sed -i '' 's/import lattice_apiFFI/import lattice_apiFFI\nimport lattice_bindingsFFI/' "$APP_DIR/Bindings/lattice_api.swift"

echo ""
echo "Setup complete!"
