import SwiftUI

/// Debug view for store status, history, and sync operations
struct StoreDebugView: View {
    @EnvironmentObject var latticeService: LatticeService
    @Environment(\.dismiss) var dismiss
    let store: StoreInfo
    let meshId: String
    
    @State private var storeInfo: StoreInfo?
    @State private var history: [HistoryEntry] = []
    @State private var isLoadingStatus = false
    @State private var isLoadingHistory = false
    @State private var isSyncing = false
    
    var body: some View {
        Form {
            Section("Store Info") {
                LabeledContent("ID", value: store.idString)
                LabeledContent("Type", value: store.storeType)
                if !store.name.isEmpty {
                    LabeledContent("Name", value: store.name)
                }
                LabeledContent("Archived", value: store.archived ? "Yes" : "No")
            }
            
            if let details = storeInfo?.details {
                Section("Status") {
                    LabeledContent("Authors", value: "\(details.authorCount)")
                    LabeledContent("Log Files", value: "\(details.logFileCount)")
                    LabeledContent("Log Size", value: formatBytes(details.logBytes))
                    LabeledContent("Orphans", value: "\(details.orphanCount)")
                }
            }
            
        }
        #if os(macOS)
        .formStyle(.grouped)
        .frame(minWidth: 300, minHeight: 200)
        #endif
        .navigationTitle("Store Info")
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Close") { dismiss() }
            }
        }
        .task(id: store.id) {
            storeInfo = nil
            await loadStatus()
        }
    }
    
    private func loadStatus() async {
        isLoadingStatus = true
        defer { isLoadingStatus = false }
        do {
            storeInfo = try await latticeService.getStoreStatus(storeId: store.idString)
        } catch {
            // Ignore
        }
    }
    
    private func formatBytes(_ bytes: UInt64) -> String {
        let formatter = ByteCountFormatter()
        formatter.countStyle = .file
        return formatter.string(fromByteCount: Int64(bytes))
    }
}
