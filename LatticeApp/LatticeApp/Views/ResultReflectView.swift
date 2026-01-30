import SwiftUI
#if os(macOS)
import AppKit
#elseif os(iOS)
import UIKit
#endif

struct ResultReflectView: View {
    let data: Data
    var registry: SchemaRegistry? = nil
    var serviceName: String? = nil
    var methodName: String? = nil
    
    var descriptor: MessageDef? {
        guard let registry, let serviceName, let methodName else { return nil }
        
        // Find service by name or suffix
        if let service = registry.services[serviceName] ?? registry.services.values.first(where: { $0.name == serviceName || $0.fullName.hasSuffix("." + serviceName) }) {
             // Find method
             if let method = service.methods[methodName] {
                 // Find output message type
                 // outputType is full name (e.g. .package.Msg)
                 // Registry keys are "package.Msg" (maybe without leading dot if parsed that way? logic in parser was slightly loose on dot)
                 let type = method.outputType
                 let key = type.hasPrefix(".") ? String(type.dropFirst()) : type
                 // Also try with leading dot
                 return registry.messages[key] ?? registry.messages["." + key] ?? registry.messages[type]
             }
        }
        return nil
    }
    
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            headerWithCopy
            
            Divider()
            
            if data.isEmpty {
                Text("Success (Empty response)")
                    .foregroundStyle(.secondary)
            } else {
                VStack(alignment: .leading, spacing: 12) {
                    
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Structure")
                            .font(.caption).foregroundStyle(.secondary)
                        ProtobufViewer(data: data, messageDef: descriptor, registry: registry)
                            .padding(8)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(Color.gray.opacity(0.1))
                            .cornerRadius(8)
                    }

                    if let string = String(data: data, encoding: .utf8), !string.contains(where: { char in
                        char.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) && !char.isWhitespace
                    }) {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("UTF-8")
                                .font(.caption).foregroundStyle(.secondary)
                            Text(string)
                                .monospaced()
                                .textSelection(.enabled)
                        }
                    }
                }
            }
        }
        .padding(.vertical, 4)
    }
    
    private var headerWithCopy: some View {
        HStack {
            Text("Result")
            if registry != nil {
                Image(systemName: "checkmark.shield")
                    .foregroundStyle(.green)
                    .help("Schema Verified")
            } else {
                Image(systemName: "exclamationmark.shield")
                    .foregroundStyle(.orange)
                    .help("Schema Missing")
            }
            
            Spacer()
            if !data.isEmpty {
                 Menu {
                     Button("Copy Hex") { copyToClipboard(hex: true) }
                     if String(data: data, encoding: .utf8) != nil {
                         Button("Copy String") { copyToClipboard(hex: false) }
                     }
                 } label: {
                     Image(systemName: "doc.on.doc")
                        .font(.caption)
                 }
                 .buttonStyle(.borderless)
            }
        }
    }
    
    private func copyToClipboard(hex: Bool) {
        let text: String
        if hex {
            text = data.map { String(format: "%02x", $0) }.joined()
        } else {
            text = String(data: data, encoding: .utf8) ?? ""
        }
        
        #if os(macOS)
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)
        #elseif os(iOS)
        UIPasteboard.general.string = text
        #endif
    }
}
