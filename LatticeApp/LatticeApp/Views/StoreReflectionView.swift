import SwiftUI

struct StoreReflectionView: View {
    @EnvironmentObject var latticeService: LatticeService
    let store: StoreInfo
    let meshId: String
    
    @State private var methods: [MethodInfo] = []
    @State private var isLoading = false
    @State private var errorMsg: String?
    @State private var showDebug = false
    @State private var showStreams = false
    @State private var selectedMethod: MethodInfo?
    
    var body: some View {
        VStack(spacing: 0) {
            // Header & Selection
            VStack(spacing: 12) {
                // Store Info
                HStack {
                    VStack(alignment: .leading) {
                        Text(store.name.isEmpty ? "Store" : store.name)
                            .font(.headline)
                        Text(store.idString)
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    Spacer()
                    Text(store.storeType.uppercased())
                        .font(.caption)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .background(Color.secondary.opacity(0.1))
                        .cornerRadius(6)
                }
                
                // Method Selector
                if isLoading {
                    ProgressView()
                } else if let errorMsg {
                    Text(errorMsg).foregroundStyle(.red).font(.caption)
                } else {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            ForEach(methods, id: \.name) { method in
                                Button {
                                    selectedMethod = method
                                } label: {
                                    Text(method.name)
                                        .font(.subheadline)
                                        .fontWeight(selectedMethod?.name == method.name ? .semibold : .regular)
                                        .padding(.horizontal, 12)
                                        .padding(.vertical, 6)
                                        .background(selectedMethod?.name == method.name ? Color.accentColor : Color.secondary.opacity(0.1))
                                        .foregroundStyle(selectedMethod?.name == method.name ? .white : .primary)
                                        .clipShape(Capsule())
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                }
            }
            .padding()
            .background(.regularMaterial)
            .shadow(color: .black.opacity(0.05), radius: 5, y: 5)
            .zIndex(1)
            
            // Console Area
            if let method = selectedMethod {
                MethodExecutionView(
                    storeId: store.idString,
                    method: method,
                    allMethods: methods
                )
                // Force a fresh view identity when method changes to reset internal state
                // Use combined store+method identifier to avoid collision if same method name exists in different stores
                .id("\(store.idString):\(method.name)") 
                .transition(.opacity)
            } else {
                Spacer()
                ContentUnavailableView(
                    "No Command Selected",
                    systemImage: "terminal",
                    description: Text("Select a command from the menu above to execute.")
                )
                Spacer()
            }
        }
        .navigationTitle("")
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button {
                    showStreams = true
                } label: {
                    Label("Streams", systemImage: "waveform.path")
                }
                
                Button {
                    Task {
                        try? await latticeService.syncStore(storeId: store.idString)
                    }
                } label: {
                    Label("Sync", systemImage: "arrow.triangle.2.circlepath")
                }
                
                Button {
                    showDebug = true
                } label: {
                    Label("Info", systemImage: "info.circle")
                }
            }
        }
        .sheet(isPresented: $showStreams) {
            NavigationStack {
                StreamSubscriptionView(storeId: store.idString)
                    .environmentObject(latticeService)
            }
        }
        .sheet(isPresented: $showDebug) {
            NavigationStack {
                StoreDebugView(store: store, meshId: meshId)
            }
            .environmentObject(latticeService)
        }
        .task(id: store.id) {
            methods = []
            errorMsg = nil
            await loadMethods()
        }
    }
    
    private func loadMethods() async {
        isLoading = true
        defer { isLoading = false }
        do {
            methods = try await latticeService.inspectStore(storeId: store.idString)
            // Auto-select first method if available? Or maybe 'get' if available?
            // Let's keep it empty to force explicit choice, or select first to save a tap.
            // User hates clicks. Let's select the first one.
            selectedMethod = methods.first
        } catch {
            errorMsg = error.localizedDescription
        }
    }
}


