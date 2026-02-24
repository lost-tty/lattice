#!/usr/bin/env bash
# Generate documentation artifacts from infrastructure scripts.
# Run from the workspace root: bash scripts/gen-docs.sh

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "Generating dependency graph for Hugo..."
mkdir -p "$ROOT/docs/data"
python3 "$ROOT/scripts/deps.py" --hugo > "$ROOT/docs/data/deps.md"

echo "Done. Generated docs/data/deps.md"
