#!/usr/bin/env python3
"""Lattice inter-crate dependency graph analyzer.

Usage:
    python3 scripts/deps.py          # text summary
    python3 scripts/deps.py --dot    # Graphviz DOT output (pipe to `dot -Tsvg`)
    python3 scripts/deps.py --layers # show dependency layers (topological)
    python3 scripts/deps.py --mermaid # Mermaid diagram (for Hugo/Markdown)
    python3 scripts/deps.py --hugo   # full Hugo-ready Markdown snippet
"""

import json, subprocess, sys, os
from collections import defaultdict

def load_metadata():
    # Always run cargo from the workspace root (one level up from scripts/)
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        capture_output=True, text=True, cwd=root
    )
    if result.returncode != 0:
        print("cargo metadata failed:", result.stderr, file=sys.stderr)
        sys.exit(1)
    return json.loads(result.stdout)

def extract_deps(meta):
    workspace = {p["name"] for p in meta["packages"]}
    graph = {}  # name -> {"deps": [...], "dev": [...]}
    for pkg in sorted(meta["packages"], key=lambda p: p["name"]):
        name = pkg["name"]
        normal, dev = [], []
        for dep in pkg["dependencies"]:
            if dep["name"] in workspace and dep["name"] != name:
                if dep.get("kind") == "dev":
                    dev.append(dep["name"])
                else:
                    normal.append(dep["name"])
        graph[name] = {"deps": sorted(set(normal)), "dev": sorted(set(dev))}
    return graph

# ---------------------------------------------------------------------------
# Grouping (shared across output modes)
# ---------------------------------------------------------------------------

GROUPS = {
    "core": ["lattice-model", "lattice-store-base", "lattice-proto"],
    "kernel": ["lattice-kernel", "lattice-sync"],
    "stores": ["lattice-kvstore", "lattice-kvstore-api", "lattice-kvstore-client",
               "lattice-logstore", "lattice-systemstore", "lattice-storage"],
    "net": ["lattice-net", "lattice-net-types", "lattice-net-iroh"],
    "app": ["lattice-node", "lattice-api", "lattice-runtime",
            "lattice-cli", "lattice-daemon", "lattice-bindings"],
    "test": ["lattice-mockkernel", "lattice-net-sim"],
}

GROUP_COLORS = {
    "core": "#4CAF50",
    "kernel": "#2196F3",
    "stores": "#FF9800",
    "net": "#E91E63",
    "app": "#9C27B0",
    "test": "#607D8B",
}

MERMAID_STYLES = {
    "core": "fill:#4CAF5030,stroke:#4CAF50",
    "kernel": "fill:#2196F330,stroke:#2196F3",
    "stores": "fill:#FF980030,stroke:#FF9800",
    "net": "fill:#E91E6330,stroke:#E91E63",
    "app": "fill:#9C27B030,stroke:#9C27B0",
    "test": "fill:#607D8B30,stroke:#607D8B",
}

def _crate_group(name):
    for group, crates in GROUPS.items():
        if name in crates:
            return group
    return None

# ---------------------------------------------------------------------------
# Topological layers (Kahn's algorithm)
# ---------------------------------------------------------------------------

def compute_layers(graph):
    """Topological sort into layers (Kahn's algorithm)."""
    in_degree = {name: 0 for name in graph}
    for name in graph:
        for dep in graph[name]["deps"]:
            if dep in graph:
                in_degree[name] += 1

    layers = []
    remaining = dict(in_degree)
    while remaining:
        layer = [n for n, d in remaining.items() if d == 0]
        if not layer:
            layer = sorted(remaining.keys())[:1]  # break cycle
        layers.append(sorted(layer))
        for n in layer:
            del remaining[n]
            for other in remaining:
                if n in graph.get(other, {}).get("deps", []):
                    remaining[other] -= 1
    return layers

# ---------------------------------------------------------------------------
# Output: plain text
# ---------------------------------------------------------------------------

def print_text(graph):
    print("# Lattice Inter-Crate Dependencies\n")
    for name, info in sorted(graph.items()):
        if info["deps"] or info["dev"]:
            print(f"{name}")
            if info["deps"]:
                print(f"  ← {', '.join(info['deps'])}")
            if info["dev"]:
                print(f"  ← (dev) {', '.join(info['dev'])}")
            print()

def print_layers(graph):
    layers = compute_layers(graph)
    print("# Dependency Layers (leaves first)\n")
    for i, layer in enumerate(layers):
        crates = "  ".join(layer)
        print(f"  L{i}: {crates}")
    print()

    # Reverse deps: who depends on me?
    reverse = defaultdict(list)
    for name, info in graph.items():
        for dep in info["deps"]:
            reverse[dep].append(name)

    print("# Reverse Dependencies (who depends on X?)\n")
    for name in sorted(reverse.keys()):
        dependents = sorted(reverse[name])
        print(f"  {name} ← {', '.join(dependents)}")

# ---------------------------------------------------------------------------
# Output: Graphviz DOT
# ---------------------------------------------------------------------------

def print_dot(graph):
    print("digraph lattice {")
    print('  rankdir=BT;')
    print('  node [shape=box, fontname="Helvetica", fontsize=10];')
    print('  edge [color="#666666"];')
    print()

    for group, crates in GROUPS.items():
        color = GROUP_COLORS[group]
        print(f'  // {group}')
        for c in crates:
            if c in graph:
                print(f'  "{c}" [style=filled, fillcolor="{color}30", color="{color}"];')
        print()

    for name, info in graph.items():
        for dep in info["deps"]:
            if dep in graph:
                print(f'  "{name}" -> "{dep}";')
    print("}")

# ---------------------------------------------------------------------------
# Output: Mermaid
# ---------------------------------------------------------------------------

def _mermaid_id(name):
    """Mermaid-safe node id: replace hyphens with underscores."""
    return name.replace("-", "_")

def print_mermaid(graph):
    print("%%{ init: { 'flowchart': { 'defaultRenderer': 'elk' } } }%%")
    print("graph BT")

    # Subgraphs per group
    for group, crates in GROUPS.items():
        present = [c for c in crates if c in graph]
        if not present:
            continue
        print(f"    subgraph {group}")
        for c in present:
            print(f"        {_mermaid_id(c)}[\"{c}\"]")
        print("    end")

    # Edges
    for name, info in sorted(graph.items()):
        for dep in info["deps"]:
            if dep in graph:
                print(f"    {_mermaid_id(name)} --> {_mermaid_id(dep)}")

    # Styles
    for group, crates in GROUPS.items():
        style = MERMAID_STYLES[group]
        for c in crates:
            if c in graph:
                print(f"    style {_mermaid_id(c)} {style}")

# ---------------------------------------------------------------------------
# Output: Hugo-ready Markdown
# ---------------------------------------------------------------------------

def print_hugo(graph):
    layers = compute_layers(graph)

    # Section 1: Mermaid diagram
    print("```mermaid")
    print_mermaid(graph)
    print("```")
    print()

    # Section 2: Layer table
    print("| Layer | Crates |")
    print("|-------|--------|")
    for i, layer in enumerate(layers):
        crates_str = ", ".join(f"`{c}`" for c in layer)
        print(f"| L{i} | {crates_str} |")

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    meta = load_metadata()
    graph = extract_deps(meta)

    if "--dot" in sys.argv:
        print_dot(graph)
    elif "--layers" in sys.argv:
        print_layers(graph)
    elif "--mermaid" in sys.argv:
        print_mermaid(graph)
    elif "--hugo" in sys.argv:
        print_hugo(graph)
    else:
        print_text(graph)
        print("---")
        print_layers(graph)
