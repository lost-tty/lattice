import SwiftUI

struct NodeView: View {
    @EnvironmentObject var latticeService: LatticeService
    @State private var nodeName: String = ""
    @State private var isSaving: Bool = false
    @State private var showCopied: Bool = false
    
    var body: some View {
        Form {
            Section("Node Identity") {
                HStack {
                    Text("Node ID")
                    Spacer()
                    Text(truncatedNodeId)
                        .font(.system(.body, design: .monospaced))
                        .foregroundStyle(.secondary)
                    
                    Button {
                        #if os(iOS)
                        UIPasteboard.general.string = latticeService.nodeId
                        #else
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(latticeService.nodeId, forType: .string)
                        #endif
                        withAnimation {
                            showCopied = true
                        }
                        DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
                            withAnimation {
                                showCopied = false
                            }
                        }
                    } label: {
                        Image(systemName: showCopied ? "checkmark" : "doc.on.doc")
                    }
                    .buttonStyle(.borderless)
                }
            }
            
            Section("Display Name") {
                TextField("Enter name", text: $nodeName)
                    .textFieldStyle(.roundedBorder)
                
                Button {
                    Task {
                        isSaving = true
                        defer { isSaving = false }
                        do {
                            try await latticeService.setName(nodeName)
                        } catch {
                            latticeService.errorMessage = error.localizedDescription
                        }
                    }
                } label: {
                    if isSaving {
                        ProgressView()
                            .controlSize(.small)
                    } else {
                        Text("Save Name")
                    }
                }
                .disabled(nodeName.isEmpty || isSaving)
            }
            
            if let error = latticeService.errorMessage {
                Section {
                    Text(error)
                        .foregroundStyle(.red)
                }
            }
        }
        .navigationTitle("Node")
        #if os(iOS)
        .navigationBarTitleDisplayMode(.large)
        #endif
    }
    
    private var truncatedNodeId: String {
        let id = latticeService.nodeId
        if id.count > 16 {
            return String(id.prefix(8)) + "..." + String(id.suffix(8))
        }
        return id
    }
}

#Preview {
    NavigationStack {
        NodeView()
            .environmentObject(LatticeService.shared)
    }
}
