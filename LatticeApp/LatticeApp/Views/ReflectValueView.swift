import SwiftUI

/// Renders a ReflectValue recursively with semantic formatting
struct ReflectValueView: View {
    let value: ReflectValue
    var depth: Int = 0
    var methods: [MethodInfo]? = nil
    var onRowAction: ((RowAction) -> Void)? = nil
    
    private var maxDepth: Int { 16 }
    
    var body: some View {
        if depth > maxDepth {
            Text("<max depth>")
                .foregroundStyle(.tertiary)
                .italic()
        } else {
            valueView
        }
    }
    
    @ViewBuilder
    private var valueView: some View {
        switch value {
        case .null:
            Text("null")
                .foregroundStyle(.secondary)
                .italic()
            
        case .string(let s):
            Text(s.isEmpty ? "(empty)" : s)
                .foregroundStyle(s.isEmpty ? .tertiary : .primary)
                .textSelection(.enabled)
            
        case .int(let i):
            Text(String(i))
                .monospaced()
                .foregroundStyle(.blue)
            
        case .uint(let u):
            Text(String(u))
                .monospaced()
                .foregroundStyle(.blue)
            
        case .float(let f):
            Text(String(format: "%.4f", f))
                .monospaced()
                .foregroundStyle(.cyan)
            
        case .bool(let b):
            Text(b ? "true" : "false")
                .foregroundStyle(b ? .green : .red)
            
        case .bytes(let data):
            bytesView(data)
            
        case .list(let items):
            listView(items)
            
        case .message(let fields):
            messageView(fields)
            
        case .enum(let name, let value):
            enumView(name: name, value: value)
        }
    }
    
    @ViewBuilder
    private func bytesView(_ data: Data) -> some View {
        // Try UTF-8 first (like CLI does)
        if let str = String(data: data, encoding: .utf8), isSafeString(str) {
            Text(str.isEmpty ? "(empty)" : str)
                .foregroundStyle(str.isEmpty ? .tertiary : .primary)
                .textSelection(.enabled)
        } else {
            // Fall back to hex display
            let hex = data.prefix(32).map { String(format: "%02x", $0) }.joined()
            let truncated = data.count > 32
            
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Image(systemName: "doc.text")
                        .foregroundStyle(.purple)
                    Text("\(data.count) bytes")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                
                if !data.isEmpty {
                    Text(hex + (truncated ? "…" : ""))
                        .font(.caption)
                        .monospaced()
                        .foregroundStyle(.purple.opacity(0.8))
                        .textSelection(.enabled)
                }
            }
        }
    }
    
    private func isSafeString(_ str: String) -> Bool {
        for char in str {
            if char.isNewline || char.isWhitespace { continue }
            if char.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) { return false }
        }
        return true
    }
    
    @ViewBuilder
    private func listView(_ items: [ReflectValue]) -> some View {
        if items.isEmpty {
            Text("[]")
                .foregroundStyle(.secondary)
        } else if let columns = tableColumns(for: items) {
            // Render as table for message lists
            tableView(items: items, columns: columns)
        } else {
            // Simple list for primitives
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(items.enumerated()), id: \.offset) { index, item in
                    HStack(alignment: .top, spacing: 6) {
                        Text("[\(index)]")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .frame(minWidth: 30, alignment: .trailing)
                        
                        ReflectValueView(value: item, depth: depth + 1)
                    }
                }
            }
            .padding(.leading, 8)
        }
    }
    
    /// Extract column names if all items are messages with same fields
    private func tableColumns(for items: [ReflectValue]) -> [String]? {
        guard !items.isEmpty else { return nil }
        
        var columnSet: Set<String>?
        for item in items {
            guard case .message(let fields) = item else { return nil }
            let names = Set(fields.map { $0.name })
            if columnSet == nil {
                columnSet = names
            } else if columnSet != names {
                // Different fields - can't make table
                return nil
            }
        }
        
        // Return sorted for consistent ordering
        if let cols = columnSet {
            return Array(cols).sorted()
        }
        return nil
    }
    
    @ViewBuilder
    private func tableView(items: [ReflectValue], columns: [String]) -> some View {
        // Convert to identifiable rows
        let rows = items.enumerated().compactMap { idx, item -> TableRow? in
            guard case .message(let fields) = item else { return nil }
            return TableRow(id: idx, fields: fields)
        }
        
        Table(rows) {
            TableColumnForEach(columns, id: \.self) { col in
                TableColumn(col) { (row: TableRow) in
                    let field = row.fields.first { $0.name == col }
                    Text(field.map { inlineText(for: $0.value) } ?? "-")
                }
            }
        }
        .contextMenu(forSelectionType: TableRow.ID.self) { selection in
            if let methods = methods,
               let onAction = onRowAction,
               let selectedId = selection.first,
               let row = rows.first(where: { $0.id == selectedId }) {
                let actions = ActionMatcher.findRowActions(methods: methods, rowFields: row.fields)
                if actions.isEmpty {
                    Text("No actions available")
                } else {
                    ForEach(actions) { action in
                        Button(action.displayName) {
                            onAction(action)
                        }
                    }
                }
            }
        } primaryAction: { selection in
            // Double-click: trigger first available action
            if let methods = methods,
               let onAction = onRowAction,
               let selectedId = selection.first,
               let row = rows.first(where: { $0.id == selectedId }) {
                let actions = ActionMatcher.findRowActions(methods: methods, rowFields: row.fields)
                if let first = actions.first {
                    onAction(first)
                }
            }
        }
        .frame(minHeight: CGFloat(rows.count + 1) * 28)
    }
    
    private func inlineText(for value: ReflectValue) -> String {
        switch value {
        case .null: return "null"
        case .string(let s): return s.isEmpty ? "(empty)" : s
        case .int(let i): return String(i)
        case .uint(let u): return String(u)
        case .float(let f): return String(format: "%.4f", f)
        case .bool(let b): return b ? "true" : "false"
        case .bytes(let data):
            if let str = String(data: data, encoding: .utf8), isSafeString(str) {
                return str.isEmpty ? "(empty)" : str
            }
            return "\(data.count) bytes"
        case .list(let items): return "[\(items.count) items]"
        case .message(let fields): return "{\(fields.count) fields}"
        case .enum(let name, let value): return name.isEmpty ? "\(value)" : name
        }
    }
    
    @ViewBuilder
    private func messageView(_ fields: [ReflectField]) -> some View {
        if fields.isEmpty {
            Text("{}")
                .foregroundStyle(.secondary)
        } else {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(fields, id: \.name) { field in
                    fieldRow(field)
                }
            }
            .padding(.leading, depth > 0 ? 8 : 0)
        }
    }
    
    @ViewBuilder
    private func fieldRow(_ field: ReflectField) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(field.name)
                .font(.caption)
                .foregroundStyle(.secondary)
            
            ReflectValueView(value: field.value, depth: depth + 1)
        }
        .padding(.vertical, 2)
    }
    
    @ViewBuilder
    private func enumView(name: String, value: Int32) -> some View {
        HStack(spacing: 4) {
            if !name.isEmpty {
                Text(name)
                    .foregroundStyle(.orange)
            }
            Text("(\(value))")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}

// Helper for Table rows
private struct TableRow: Identifiable {
    let id: Int
    let fields: [ReflectField]
}

// MARK: - Compact Inline View

/// A more compact single-line representation for simple values
struct ReflectValueInlineView: View {
    let value: ReflectValue
    
    var body: some View {
        Text(inlineText)
            .lineLimit(1)
            .truncationMode(.tail)
    }
    
    private var inlineText: String {
        switch value {
        case .null:
            return "null"
        case .string(let s):
            return s.isEmpty ? "(empty)" : s
        case .int(let i):
            return String(i)
        case .uint(let u):
            return String(u)
        case .float(let f):
            return String(format: "%.4f", f)
        case .bool(let b):
            return b ? "true" : "false"
        case .bytes(let data):
            // Try UTF-8 first
            if let str = String(data: data, encoding: .utf8), isSafeString(str) {
                return str.isEmpty ? "(empty)" : str
            }
            return "\(data.count) bytes"
        case .list(let items):
            return "[\(items.count) items]"
        case .message(let fields):
            return "{\(fields.count) fields}"
        case .enum(let name, let value):
            return name.isEmpty ? "\(value)" : name
        }
    }
    
    private func isSafeString(_ str: String) -> Bool {
        for char in str {
            if char.isNewline || char.isWhitespace { continue }
            if char.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) { return false }
        }
        return true
    }
}

#Preview {
    ScrollView {
        VStack(alignment: .leading, spacing: 20) {
            ReflectValueView(value: .message([
                ReflectField(name: "name", value: .string("Test")),
                ReflectField(name: "count", value: .int(42)),
                ReflectField(name: "active", value: .bool(true)),
                ReflectField(name: "status", value: .enum(name: "ACTIVE", value: 1)),
                ReflectField(name: "items", value: .list([
                    .string("one"),
                    .string("two"),
                    .string("three")
                ])),
                ReflectField(name: "nested", value: .message([
                    ReflectField(name: "inner", value: .string("nested value"))
                ]))
            ]))
        }
        .padding()
    }
}
