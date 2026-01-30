# Lattice SwiftUI App

A cross-platform SwiftUI app for iOS, macOS, and iPadOS that provides GUI access to Lattice mesh network functionality.

## Prerequisites

1. **Xcode 15+** with iOS 17 / macOS 14 SDK
2. **Rust toolchain** with required targets:
   ```bash
   rustup target add aarch64-apple-ios aarch64-apple-darwin x86_64-apple-darwin
   ```

## Automated Setup (Recommended)

Use the provided script to build the Rust library, generate bindings, and prepare the XCFramework for both iOS and macOS:

```bash
cd LatticeApp
./setup-app.sh
```

**One-Time Xcode Setup:**

1. Drag `LatticeApp/LatticeApp/LatticeBindings.xcframework` into the "Frameworks, Libraries, and Embedded Content" section of your target settings in Xcode.
2. Ensure "Embed & Sign" is selected.
3. Remove any references to `liblattice_bindings.dylib` if present (legacy).
4. **Clean Build Folder** (Product > Clean Build Folder) if you encounter build errors.

## Legacy Manual Build

(See `scripts/build-xcframework.sh` for details on how the XCFramework is constructed)

## Project Structure

```
LatticeApp/
├── LatticeApp/
│   ├── LatticeAppApp.swift
│   ├── Services/
│   ├── Views/
│   ├── lattice_bindings.swift   # Generated
│   └── LatticeBindings.xcframework
├── setup-app.sh
└── README.md
```

## Troubleshooting

### "Cannot convert value of type 'RustBuffer'..."
This means Xcode is seeing an outdated C header.
1. Run `./setup-app.sh`
2. In Xcode: **Product > Clean Build Folder**
3. Build again.

### "Library not loaded"
Ensure `LatticeBindings.xcframework` is marked as "Embed & Sign".
