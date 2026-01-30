import SwiftUI

struct ContentView: View {
    @EnvironmentObject var latticeService: LatticeService
    @State private var expandedMeshes: Set<Data> = []
    
    var body: some View {
        Group {
            switch latticeService.state {
            case .initializing:
                loadingView(text: "Initializing...")
            case .startingNode:
                loadingView(text: "Starting Node...")
            case .ready:
                mainContent
            case .failed(let error):
                errorView(message: error)
            }
        }
    }
    
    private func loadingView(text: String) -> some View {
        VStack(spacing: 16) {
            ProgressView()
                .controlSize(.large)
            Text(text)
                .foregroundStyle(.secondary)
        }
    }
    
    private func errorView(message: String) -> some View {
        VStack(spacing: 16) {
            Image(systemName: "exclamationmark.triangle")
                .font(.largeTitle)
                .foregroundStyle(.red)
            Text("Initialization Failed")
                .font(.headline)
            Text(message)
                .font(.caption)
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
        }
        .padding()
    }
    
    @ViewBuilder
    private var mainContent: some View {
        #if os(iOS)
        NavigationStack {
            DashboardView()
        }
        #else
        NavigationSplitView {
            sidebarContent
        } detail: {
            DashboardView()
        }
        #endif
    }
    
    #if os(macOS)
    private var sidebarContent: some View {
        List {
            NavigationLink {
                DashboardView()
            } label: {
                Label("Dashboard", systemImage: "gauge")
            }

            Section("Meshes") {
                ForEach(latticeService.meshes) { mesh in
                    MeshDisclosureRow(
                        mesh: mesh,
                        stores: latticeService.stores[mesh.idString] ?? [],
                        isExpanded: expandedMeshes.contains(mesh.id),
                        onExpandChange: { isExpanded in
                            if isExpanded {
                                expandedMeshes.insert(mesh.id)
                                Task { await latticeService.refreshStores(mesh: mesh) }
                            } else {
                                expandedMeshes.remove(mesh.id)
                            }
                        }
                    )
                }
            }
        }
        .navigationTitle("Lattice")
        .listStyle(.sidebar)
    }
    #endif
}

#if os(macOS)
struct MeshDisclosureRow: View {
    let mesh: MeshInfo
    let stores: [StoreInfo]
    let isExpanded: Bool
    let onExpandChange: (Bool) -> Void
    
    var body: some View {
        DisclosureGroup(
            isExpanded: Binding(
                get: { isExpanded },
                set: { onExpandChange($0) }
            )
        ) {
            storesList
        } label: {
            NavigationLink {
                MeshDetailView(mesh: mesh)
            } label: {
                Label(mesh.displayName, systemImage: "network")
            }
        }
    }
    
    @ViewBuilder
    private var storesList: some View {
        if stores.isEmpty {
            Text("No stores")
                .font(.caption)
                .foregroundStyle(.secondary)
        } else {
            ForEach(stores) { store in
                NavigationLink {
                    StoreReflectionView(store: store, meshId: mesh.idString)
                } label: {
                    Label(store.name.isEmpty ? "Store" : store.name, systemImage: "externaldrive")
                }
            }
        }
    }
}
#endif

#Preview {
    ContentView()
        .environmentObject(LatticeService.shared)
}
