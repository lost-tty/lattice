import SwiftUI

struct DashboardView: View {
    @EnvironmentObject var latticeService: LatticeService
    
    // Sheet State
    @State private var showingCreateMesh = false
    @State private var showingJoinMesh = false
    @State private var showingNodeSettings = false
    @State private var joinToken = ""
    @State private var newMeshName = ""
    
    // Grid Layout
    let columns = [
        GridItem(.adaptive(minimum: 160), spacing: 16)
    ]
    
    var body: some View {
        ScrollView {
            VStack(spacing: 24) {
                // 1. Mesh Grid
                VStack(alignment: .leading, spacing: 12) {
                    HStack {
                        Text("ACTIVE MESHES")
                            .font(.caption)
                            .fontWeight(.bold)
                            .foregroundStyle(.secondary)
                        Spacer()
                    }
                    
                    if latticeService.meshes.isEmpty {
                        emptyState
                    } else {
                        LazyVGrid(columns: columns, spacing: 16) {
                            ForEach(latticeService.meshes) { mesh in
                                NavigationLink {
                                    MeshDetailView(mesh: mesh)
                                } label: {
                                    MeshCard(mesh: mesh)
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                }
            }
            .padding()
        }
        .navigationTitle(latticeService.nodeName.isEmpty ? "Dashboard" : latticeService.nodeName)

        // Sheet logic
        .sheet(isPresented: $showingCreateMesh) {
            createMeshSheet
        }
        .sheet(isPresented: $showingNodeSettings) {
             NodeSettingsSheet()
        }
        .alert("Join Mesh", isPresented: $showingJoinMesh) {
            TextField("Invite Token", text: $joinToken)
            Button("Cancel", role: .cancel) { }
            Button("Join") {
                Task {
                    do {
                        _ = try await latticeService.joinMesh(token: joinToken)
                        joinToken = ""
                    } catch {
                        latticeService.errorMessage = error.localizedDescription
                    }
                }
            }
        }
        .refreshable {
            await latticeService.refreshMeshes()
            await latticeService.refreshNodeName()
        }
        .toolbar {
             ToolbarItemGroup(placement: .primaryAction) {
                 Button(action: { showingNodeSettings = true }) {
                     Label("Settings", systemImage: "gearshape")
                 }
                 
                 Button(action: {
                     Task {
                         await latticeService.refreshMeshes()
                         await latticeService.refreshNodeName()
                     }
                 }) {
                     Label("Refresh", systemImage: "arrow.clockwise")
                 }
                 
                 Button(action: { showingJoinMesh = true }) {
                     Label("Join", systemImage: "link")
                 }
                 Button(action: { showingCreateMesh = true }) {
                     Label("Create", systemImage: "plus")
                 }
             }
         }
    }
    
    // MARK: - Components
    

    
    private var emptyState: some View {
        VStack(spacing: 16) {
            Image(systemName: "network.slash")
                .font(.system(size: 48))
                .foregroundStyle(.secondary)
            Text("No Active Meshes")
                .font(.headline)
            Text("Join a mesh or create a new one to start syncing data.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            
            HStack {
                Button("Create Mesh") { showingCreateMesh = true }
                    .buttonStyle(.borderedProminent)
                Button("Join Mesh") { showingJoinMesh = true }
                    .buttonStyle(.bordered)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(32)
        .background(Color.primary.opacity(0.05))
        .cornerRadius(12)
    }
    
    private var createMeshSheet: some View {
        NavigationStack {
            Form {
                Text("Create a new localized mesh? You can invite others to join it later.")
                    .foregroundStyle(.secondary)
            }
            .navigationTitle("New Mesh")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { showingCreateMesh = false }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create") {
                        Task {
                            do {
                                // TODO: Add alias support in Service
                                _ = try await latticeService.createMesh()
                                showingCreateMesh = false
                            } catch {
                                latticeService.errorMessage = error.localizedDescription
                            }
                        }
                    }
                }
            }
        }
        .presentationDetents([.medium])
    }
}

struct NodeSettingsSheet: View {
    @EnvironmentObject var latticeService: LatticeService
    @Environment(\.dismiss) var dismiss
    @State private var name: String = ""
    
    var body: some View {
        NavigationStack {
            Form {
                Section("Identity") {
                    TextField("Node Name", text: $name)
                        .autocorrectionDisabled()
                        
                    LabeledContent("Node ID") {
                        Text(latticeService.nodeId)
                            .font(.caption.monospaced())
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                    }
                }
            }
            .navigationTitle("Node Settings")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        Task {
                            try? await latticeService.setName(name)
                            dismiss()
                        }
                    }
                    .disabled(name == latticeService.nodeName)
                }
            }
            .onAppear {
                name = latticeService.nodeName
            }
        }
        .presentationDetents([.medium, .large])
    }
}

// MARK: - Subcomponents

struct MeshCard: View {
    let mesh: MeshInfo
    @EnvironmentObject var service: LatticeService
    @State private var peerCount: Int = 0
    @State private var storeCount: Int = 0
    @State private var hasLoaded = false
    
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            // Header
            HStack {
                Image(systemName: "network")
                    .foregroundStyle(.blue)
                Spacer()
                Text(mesh.displayName)
                    .font(.caption2.monospaced())
                    .foregroundStyle(.secondary)
            }
            
            Text(mesh.displayName.isEmpty ? "Untitled Mesh" : mesh.displayName)
                .font(.headline)
                .lineLimit(1)
            
            Divider()
            
            // Stats
            HStack {
                HStack(spacing: 4) {
                    if peerCount > 0 {
                         Circle().fill(.green).frame(width: 6, height: 6)
                    } else {
                         Circle().fill(.gray).frame(width: 6, height: 6)
                    }
                    Text("\(peerCount)")
                }
                .font(.caption.monospaced())
                
                Spacer()
                
                HStack(spacing: 4) {
                    Image(systemName: "externaldrive")
                        .font(.caption2)
                    Text("\(storeCount)")
                }
                .font(.caption.monospaced())
            }
            .foregroundStyle(.secondary)
        }
        .padding()
        .background(Color.primary.opacity(0.03))
        .cornerRadius(10)
        .overlay(
            RoundedRectangle(cornerRadius: 10)
                .strokeBorder(Color.primary.opacity(0.1), lineWidth: 1)
        )
        .task {
            await loadStats()
        }
        // Reload when service updates
        .onChange(of: service.stores) {
            Task { await loadStats() }
        }
    }
    
    private func loadStats() async {
        if let peers = try? await service.getMeshPeers(meshId: mesh.idString) {
            peerCount = peers.count
        }
        storeCount = service.stores[mesh.idString]?.count ?? 0
    }
}

struct StatView: View {
    let label: String
    let value: String
    
    var body: some View {
        VStack(spacing: 2) {
            Text(value)
                .font(.headline.monospaced())
            Text(label)
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
    }
}

// Helper for Color cross-platform
extension Color {
    static var panelBackground: Color {
        #if os(macOS)
        return Color(nsColor: .controlBackgroundColor)
        #else
        return Color(uiColor: .secondarySystemBackground)
        #endif
    }

    #if os(macOS)
    init(nsColor: NSColor) {
        self.init(nsColor)
    }
    #endif
    
    #if os(iOS)
    init(uiColor: UIColor) {
        self.init(uiColor)
    }
    #endif
}
