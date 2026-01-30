import SwiftUI

struct MeshDetailView: View {
    @EnvironmentObject var latticeService: LatticeService
    let mesh: MeshInfo
    
    @State private var peers: [PeerInfo] = []
    @State private var stores: [StoreInfo] = []
    @State private var inviteToken: String?
    @State private var isLoadingPeers = false
    @State private var isLoadingStores = false
    @State private var showInviteSheet = false
    @State private var showCreateStoreSheet = false
    
    var body: some View {
        List {
            Section("Mesh Info") {
                LabeledContent("ID", value: mesh.displayName + "...")
                LabeledContent("Peers", value: "\(mesh.peerCount)")
                LabeledContent("Stores", value: "\(mesh.storeCount)")
            }
            
            Section("Stores") {
                if isLoadingStores {
                    ProgressView()
                } else if stores.isEmpty {
                    Text("No stores")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(stores, id: \.id) { store in
                        NavigationLink {
                            StoreReflectionView(store: store, meshId: mesh.idString)
                        } label: {
                            StoreRow(store: store)
                        }
                    }
                }
            }
            
            Section("Peers") {
                if isLoadingPeers {
                    ProgressView()
                } else if peers.isEmpty {
                    Text("No peers")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(peers, id: \.publicKey) { peer in
                        PeerRow(peer: peer)
                    }
                }
            }
        }
        .navigationTitle(mesh.displayName)
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button {
                    showInviteSheet = true
                    Task {
                        do {
                            inviteToken = try await latticeService.getMeshInvite(meshId: mesh.idString)
                        } catch {
                            latticeService.errorMessage = error.localizedDescription
                        }
                    }
                } label: {
                    Label("Invite", systemImage: "person.badge.plus")
                }
                
                Button {
                    showCreateStoreSheet = true
                } label: {
                    Label("Add Store", systemImage: "plus")
                }
            }
        }
        .refreshable {
            await loadData()
        }
        .task(id: mesh.id) {
            // Reset state when mesh changes
            peers = []
            stores = []
            inviteToken = nil
            await loadData()
        }
        .sheet(isPresented: $showInviteSheet) {
            InviteSheet(token: inviteToken)
        }
        .sheet(isPresented: $showCreateStoreSheet) {
            CreateStoreSheet(meshId: mesh.idString) {
                Task { await loadStores() }
            }
        }
    }
    
    private func loadData() async {
        await withTaskGroup(of: Void.self) { group in
            group.addTask { await loadPeers() }
            group.addTask { await loadStores() }
        }
    }
    
    private func loadPeers() async {
        isLoadingPeers = true
        defer { isLoadingPeers = false }
        do {
            // Use optimized overload taking MeshInfo directly
            peers = try await latticeService.getMeshPeers(meshId: mesh.idString)
        } catch {
            // Ignore for now
        }
    }
    
    private func loadStores() async {
        isLoadingStores = true
        defer { isLoadingStores = false }
        do {
            // Use optimized overload taking MeshInfo directly
            stores = try await latticeService.getStores(mesh: mesh)
        } catch {
            // Ignore for now
        }
    }
}

struct PeerRow: View {
    let peer: PeerInfo
    
    var body: some View {
        HStack {
            Circle()
                .fill(peer.online ? .green : .gray)
                .frame(width: 10, height: 10)
            
            VStack(alignment: .leading) {
                Text(peer.name.isEmpty ? "Unknown" : peer.name)
                    .font(.headline)
                Text(peerIdString)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            
            Spacer()
            
            Text(peer.status)
                .font(.caption)
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
                .background(.secondary.opacity(0.2))
                .clipShape(Capsule())
        }
    }
    
    private var peerIdString: String {
        let hex = peer.publicKey.map { String(format: "%02x", $0) }.joined()
        return String(hex.prefix(8)) + "..."
    }
}

struct StoreRow: View {
    let store: StoreInfo
    
    var body: some View {
        VStack(alignment: .leading) {
            Text(store.name.isEmpty ? store.idString.prefix(8) + "..." : store.name)
                .font(.headline)
            Text(store.storeType)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}

struct InviteSheet: View {
    @Environment(\.dismiss) var dismiss
    let token: String?
    @State private var showCopied = false
    
    var body: some View {
        NavigationStack {
            VStack(spacing: 20) {
                if let token {
                    Text("Share this invite token with others to let them join your mesh.")
                        .multilineTextAlignment(.center)
                        .foregroundStyle(.secondary)
                    
                    Text(token)
                        .font(.system(.body, design: .monospaced))
                        .padding()
                        .background(.secondary.opacity(0.1))
                        .clipShape(RoundedRectangle(cornerRadius: 8))
                        .textSelection(.enabled)
                    
                    Button {
                        #if os(iOS)
                        UIPasteboard.general.string = token
                        #else
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(token, forType: .string)
                        #endif
                        showCopied = true
                    } label: {
                        Label(showCopied ? "Copied!" : "Copy Token", systemImage: showCopied ? "checkmark" : "doc.on.doc")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent)
                } else {
                    ProgressView("Generating invite...")
                }
            }
            .padding()
            .navigationTitle("Invite")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium])
    }
}

struct CreateStoreSheet: View {
    @EnvironmentObject var latticeService: LatticeService
    @Environment(\.dismiss) var dismiss
    let meshId: String
    let onCreated: () -> Void
    
    @State private var storeName = ""
    @State private var storeType = "kv"
    @State private var isCreating = false
    
    let storeTypes = ["kv", "log", "counter"]
    
    var body: some View {
        NavigationStack {
            Form {
                Section("Store Details") {
                    TextField("Name (optional)", text: $storeName)
                    
                    Picker("Type", selection: $storeType) {
                        ForEach(storeTypes, id: \.self) { type in
                            Text(type).tag(type)
                        }
                    }
                }
                
                Section {
                    Button {
                        Task {
                            isCreating = true
                            defer { isCreating = false }
                            do {
                                _ = try await latticeService.createStore(
                                    meshId: meshId,
                                    name: storeName.isEmpty ? nil : storeName,
                                    storeType: storeType
                                )
                                onCreated()
                                dismiss()
                            } catch {
                                latticeService.errorMessage = error.localizedDescription
                            }
                        }
                    } label: {
                        if isCreating {
                            ProgressView()
                                .frame(maxWidth: .infinity)
                        } else {
                            Text("Create Store")
                                .frame(maxWidth: .infinity)
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(isCreating)
                }
            }
            .navigationTitle("New Store")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
    }
}

#Preview {
    NavigationStack {
        MeshDetailView(mesh: MeshInfo(id: Data([0x01, 0x02, 0x03, 0x04]), peerCount: 2, storeCount: 3))
            .environmentObject(LatticeService.shared)
    }
}
