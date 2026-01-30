import SwiftUI

struct MeshListView: View {
    @EnvironmentObject var latticeService: LatticeService
    @State private var showCreateSheet = false
    @State private var showJoinSheet = false
    @State private var joinToken = ""
    @State private var isJoining = false
    
    var body: some View {
        List {
            if latticeService.state != .ready {
                ProgressView("Loading...")
            } else if latticeService.meshes.isEmpty {
                ContentUnavailableView {
                    Label("No Meshes", systemImage: "network.slash")
                } description: {
                    Text("Create a new mesh or join an existing one.")
                } actions: {
                    Button("Create Mesh") {
                        showCreateSheet = true
                    }
                    .buttonStyle(.borderedProminent)
                }
            } else {
                ForEach(latticeService.meshes, id: \.id) { mesh in
                    NavigationLink {
                        MeshDetailView(mesh: mesh)
                    } label: {
                        MeshRow(mesh: mesh)
                    }
                }
            }
        }
        .navigationTitle("Meshes")
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button {
                    showJoinSheet = true
                } label: {
                    Label("Join", systemImage: "person.badge.plus")
                }
                
                Button {
                    showCreateSheet = true
                } label: {
                    Label("Create", systemImage: "plus")
                }
            }
            
            ToolbarItem(placement: .automatic) {
                Button {
                    Task {
                        await latticeService.refreshMeshes()
                    }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
            }
        }
        .refreshable {
            await latticeService.refreshMeshes()
        }
        .sheet(isPresented: $showCreateSheet) {
            CreateMeshSheet()
        }
        .sheet(isPresented: $showJoinSheet) {
            JoinMeshSheet()
        }
    }
}

struct MeshRow: View {
    let mesh: MeshInfo
    
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(mesh.displayName)
                .font(.headline)
            
            HStack(spacing: 12) {
                Label("\(mesh.peerCount)", systemImage: "person.2")
                Label("\(mesh.storeCount)", systemImage: "externaldrive")
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
        .padding(.vertical, 4)
    }
}

struct CreateMeshSheet: View {
    @EnvironmentObject var latticeService: LatticeService
    @Environment(\.dismiss) var dismiss
    @State private var isCreating = false
    
    var body: some View {
        NavigationStack {
            VStack(spacing: 20) {
                Image(systemName: "network")
                    .font(.system(size: 60))
                    .foregroundStyle(.tint)
                
                Text("Create New Mesh")
                    .font(.title2)
                
                Text("A new mesh will be created and you'll become its first member.")
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
                
                Button {
                    Task {
                        isCreating = true
                        defer { isCreating = false }
                        do {
                            _ = try await latticeService.createMesh()
                            dismiss()
                        } catch {
                            latticeService.errorMessage = error.localizedDescription
                        }
                    }
                } label: {
                    if isCreating {
                        ProgressView()
                    } else {
                        Text("Create Mesh")
                            .frame(maxWidth: .infinity)
                    }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(isCreating)
            }
            .padding()
            .navigationTitle("New Mesh")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium])
    }
}

struct JoinMeshSheet: View {
    @EnvironmentObject var latticeService: LatticeService
    @Environment(\.dismiss) var dismiss
    @State private var token = ""
    @State private var isJoining = false
    
    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Invite Token", text: $token)
                        .textFieldStyle(.roundedBorder)
                        .autocorrectionDisabled()
                        #if os(iOS)
                        .textInputAutocapitalization(.never)
                        #endif
                } header: {
                    Text("Enter Invite Token")
                } footer: {
                    Text("Paste the invite token you received from another mesh member.")
                }
                
                Section {
                    Button {
                        Task {
                            isJoining = true
                            defer { isJoining = false }
                            do {
                                _ = try await latticeService.joinMesh(token: token)
                                dismiss()
                            } catch {
                                latticeService.errorMessage = error.localizedDescription
                            }
                        }
                    } label: {
                        if isJoining {
                            ProgressView()
                                .frame(maxWidth: .infinity)
                        } else {
                            Text("Join Mesh")
                                .frame(maxWidth: .infinity)
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(token.isEmpty || isJoining)
                }
            }
            .navigationTitle("Join Mesh")
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
        MeshListView()
            .environmentObject(LatticeService.shared)
    }
}
