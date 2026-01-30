import SwiftUI

struct ProtobufViewer: View {
    let data: Data
    var messageDef: MessageDef? = nil
    var registry: SchemaRegistry? = nil
    
    var body: some View {
        let fields = ProtobufDecoder.decode(data)
        if fields.isEmpty {
            Text("Empty or Invalid Protobuf")
                .foregroundStyle(.secondary)
        } else {
            VStack(alignment: .leading, spacing: 1) {
                ForEach(fields) { field in
                    // Find field def
                    let fieldDef = messageDef?.fields[field.fieldNumber]
                    ProtobufFieldView(field: field, fieldDef: fieldDef, registry: registry)
                    Divider()
                        .opacity(0.5)
                }
            }
        }
    }
}

struct ProtobufFieldView: View {
    let field: ProtoField
    var fieldDef: FieldDef? = nil
    var registry: SchemaRegistry? = nil
    @State private var isExpanded = true
    
    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .top) {
                // Name or Number
                if let name = fieldDef?.name {
                   Text("\(name):")
                        .font(.caption)
                        .foregroundStyle(.primary)
                } else {
                   Text("\(field.fieldNumber):")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .monospacedDigit()
                }
                
                contentView
            }

        }
    }
    
    @ViewBuilder
    var contentView: some View {
        if let def = fieldDef {
            // Strict Schema Mode
            schemaContentView(def: def)
        } else {
            // Heuristic Mode
            heuristicContentView
        }
    }
    
    @ViewBuilder
    func schemaContentView(def: FieldDef) -> some View {
        switch def.type {
        case 9: // String
             if let str = field.asString {
                 Text("\"\(str)\"")
                    .monospaced()
                    .foregroundStyle(.orange)
                    .lineLimit(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
             } else {
                 Text("Invalid String").foregroundStyle(.red)
             }
        case 11: // Message
             if let data = field.asData, let typeName = def.typeName, let registry = registry {
                 // Resolve nested type
                 // typeName usually has leading dot. Removing it?
                 // Registry uses full name.
                 let msgDef = registry.messages[typeName] ?? registry.messages[typeName.hasPrefix(".") ? String(typeName.dropFirst()) : "." + typeName]
                 let subFields = ProtobufDecoder.decode(data)
                 
                 if !subFields.isEmpty {
                      CollapsibleMessageView(data: data, subFields: subFields, startExpanded: true, messageDef: msgDef, registry: registry)
                 } else {
                      Text("Empty Message").foregroundStyle(.secondary)
                 }
             } else {
                 Text("Message (?)")
             }
        default:
             heuristicContentView // Fallback for primitives/others
        }
    }
    
    // Original heuristic logic
    @ViewBuilder
    var heuristicContentView: some View {
        switch field.wireType {
        case .varint:
            Text("\(field.asVarint ?? 0)")
                .monospaced()
                .foregroundStyle(.blue)
        case .fixed64:
             if let data = field.value as? Data {
                 Text("0x" + data.map { String(format: "%02x", $0) }.joined())
                    .monospaced()
                    .foregroundStyle(.purple)
             }
        case .fixed32:
             if let data = field.value as? Data {
                 Text("0x" + data.map { String(format: "%02x", $0) }.joined())
                    .monospaced()
                    .foregroundStyle(.purple)
             }
        case .lengthDelimited:
            if let data = field.asData {
                // Strict Mode: No guessing sub-messages
                // But try guessing String (CLI behavior)
                if let str = String(data: data, encoding: .utf8), isSafeString(str) {
                     Text("\"\(str)\"")
                        .monospaced()
                        .foregroundStyle(.orange)
                        .lineLimit(10)
                        .frame(maxWidth: .infinity, alignment: .leading)
                } else {
                     Text("Bytes (\(data.count)) " + data.prefix(8).map { String(format: "%02x", $0) }.joined() + "...")
                        .monospaced()
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        default:
            Text("Type \(field.wireType.rawValue)")
        }
    }
    
    func isSafeString(_ str: String) -> Bool {
        for char in str {
            if char.isNewline || char.isWhitespace { continue }
            if char.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) { return false }
        }
        return true
    }
}

// Helper view for collapsible message
struct CollapsibleMessageView: View {
    let data: Data
    let subFields: [ProtoField]
    let messageDef: MessageDef?
    let registry: SchemaRegistry?
    @State var isExpanded: Bool
    
    init(data: Data, subFields: [ProtoField], startExpanded: Bool, messageDef: MessageDef? = nil, registry: SchemaRegistry? = nil) {
        self.data = data
        self.subFields = subFields
        self.messageDef = messageDef
        self.registry = registry
        self._isExpanded = State(initialValue: startExpanded)
    }
    
    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Button(action: { withAnimation(.snappy) { isExpanded.toggle() } }) {
                HStack(spacing: 4) {
                    Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    Text(titleText)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .padding(.vertical, 2)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            
            if isExpanded {
                VStack(alignment: .leading, spacing: 1) {
                    ForEach(subFields) { sub in
                        ProtobufFieldView(field: sub, fieldDef: messageDef?.fields[sub.fieldNumber], registry: registry)
                    }
                }
                .padding(.leading, 4)
            }
        }
    }
    
    var titleText: String {
        if let name = messageDef?.name {
            return "\(name) (\(subFields.count) fields)"
        }
        return "Structure (\(subFields.count) fields)"
    }
}
