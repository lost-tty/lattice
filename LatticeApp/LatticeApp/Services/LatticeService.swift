import Foundation
import Combine
#if os(iOS)
import UIKit
#endif

// MARK: - Extensions

extension MeshInfo: Identifiable {
    /// Display-friendly mesh ID (first 8 chars of hex)
    public var displayName: String {
        let hex = id.map { String(format: "%02x", $0) }.joined()
        return String(hex.prefix(8))
    }
    
    /// Full hex string of mesh ID
    public var idString: String {
        id.map { String(format: "%02x", $0) }.joined()
    }
}

extension StoreInfo: Identifiable {
    /// Full hex string of store ID  
    public var idString: String {
        id.map { String(format: "%02x", $0) }.joined()
    }
}

// MARK: - Lattice Service

/// Singleton service wrapping the Lattice FFI bindings.
/// Provides async-friendly methods and published state for SwiftUI.
@MainActor
class LatticeService: ObservableObject {
    static let shared = LatticeService()
    
    // MARK: - Published State
    
    enum ServiceState: Equatable {
        case initializing
        case startingNode
        case ready
        case failed(String)
    }
    
    @Published var state: ServiceState = .initializing
    
    @Published var nodeId: String = ""
    @Published var nodeName: String = ""
    @Published var meshes: [MeshInfo] = []
    @Published var stores: [String: [StoreInfo]] = [:]
    @Published var errorMessage: String?
    
    // MARK: - Private
    private var lattice: Lattice?
    private var eventTask: Task<Void, Never>?
    
    // Explicit background queue for blocking FFI calls
    private let ffiQueue = DispatchQueue(label: "com.lattice.ffi", qos: .default, attributes: .concurrent)
    
    private init() {
        // Start initialization immediately
        Task {
            await initialize()
        }
    }
    
    // MARK: - Initialization
    
    // MARK: - Initialization
    
    private func initialize() async {
        state = .initializing
        print("[Lattice] Initializing...")
        
        do {
            // Create Lattice instance (lightweight)
            // Perform on background queue
            lattice = await withCheckedContinuation { continuation in
                ffiQueue.async {
                    print("[Lattice] Creating instance on FFI queue...")
                    // Lattice() constructor is safe and doesn't throw in Rust binding
                    let l = Lattice(dataDir: nil)
                    print("[Lattice] Instance created")
                    continuation.resume(returning: l)
                }
            }
            
            guard let latticeInstance = lattice else { 
                print("[Lattice] Failed to create instance")
                state = .failed("Failed to create Lattice instance")
                return 
            }
            
            // Determine Device Name (MainActor safe)
            var deviceName: String?
            #if os(iOS)
            deviceName = UIDevice.current.name
            #elseif os(macOS)
            deviceName = Host.current().localizedName
            #endif
            
            state = .startingNode
            
            // Start the node (blocking sync call in Rust)
            // Wrap in explicit background queue to guarantee off-main-thread execution
            print("[Lattice] Starting node with name: \(deviceName ?? "unknown")...")
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                ffiQueue.async {
                    do {
                        // Initialize Core (File I/O on background)
                        let dataDir = try LatticeService.setupAppDirectory()
                        let dataDirPath = dataDir.path
                        print("[Lattice] Data dir: \(dataDirPath)")
                        
                        try latticeInstance.start(dataDir: dataDirPath, name: deviceName)
                        print("[Lattice] Node started")
                        continuation.resume()
                    } catch {
                        print("[Lattice] Start error: \(error)")
                        continuation.resume(throwing: error)
                    }
                }
            }
            print("[Lattice] Getting Node ID...")
            nodeId = try await withCheckedThrowingContinuation { continuation in
                ffiQueue.async {
                    do {
                        let id = try latticeInstance.nodeId()
                        print("[Lattice] Node ID: \(id)")
                        continuation.resume(returning: id)
                    } catch {
                        print("[Lattice] Node ID error: \(error)")
                        continuation.resume(throwing: error)
                    }
                }
            }
            
            // Node Name
            await refreshNodeName()
            
            await refreshMeshes() // This uses ffiQueue internally
            startEventListener()
            
            state = .ready
            print("[Lattice] Initialization Complete")
        } catch {
            print("[Lattice] Init Failed: \(error)")
            state = .failed("Failed to initialize: \(error.localizedDescription)")
            errorMessage = error.localizedDescription
        }
    }
    
    private nonisolated static func setupAppDirectory() throws -> URL {
        let fileManager = FileManager.default
        let docs = fileManager.urls(for: .documentDirectory, in: .userDomainMask)[0]
        let dataDir = docs.appendingPathComponent("LatticeData")
        try fileManager.createDirectory(at: dataDir, withIntermediateDirectories: true, attributes: nil)
        return dataDir
    }
    
    // ...
    
    // MARK: - Node
    
    func refreshNodeName() async {
        guard let lattice else { return }
        let latticeInstance = lattice
        do {
            nodeName = try await withCheckedThrowingContinuation { continuation in
                ffiQueue.async {
                    do {
                        let status = try latticeInstance.nodeStatus()
                        let name = status.displayName.isEmpty ? "Unknown" : status.displayName
                        continuation.resume(returning: name)
                    } catch {
                        continuation.resume(throwing: error)
                    }
                }
            }
        } catch {
             print("[Lattice] Failed to fetch node name: \(error)")
        }
    }
    
    func setName(_ name: String) async throws {
        guard let lattice else { throw LatticeServiceError.notInitialized }
        let latticeInstance = lattice
        
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            ffiQueue.async {
                do {
                    try latticeInstance.setName(name: name)
                    continuation.resume()
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
        await refreshNodeName()
    }
    
    // MARK: - Meshes
    
    func refreshMeshes() async {
        guard let lattice else { return }
        let latticeInstance = lattice
        do {
            meshes = try await withCheckedThrowingContinuation { continuation in
                ffiQueue.async {
                    do {
                        let res = try latticeInstance.meshList()
                        continuation.resume(returning: res)
                    } catch {
                        continuation.resume(throwing: error)
                    }
                }
            }
        } catch {
            errorMessage = "Failed to load meshes: \(error.localizedDescription)"
        }
    }
    
    func createMesh() async throws -> MeshInfo {
        guard let lattice else { throw LatticeServiceError.notInitialized }
        let latticeInstance = lattice
        let mesh = try await withCheckedThrowingContinuation { continuation in
            ffiQueue.async {
                do {
                    let res = try latticeInstance.meshCreate()
                    continuation.resume(returning: res)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
        await refreshMeshes()
        return mesh
    }
    
    func joinMesh(token: String) async throws -> JoinResponse {
        guard let lattice else { throw LatticeServiceError.notInitialized }
        let latticeInstance = lattice
        let result = try await withCheckedThrowingContinuation { continuation in
            ffiQueue.async {
                do {
                    let res = try latticeInstance.meshJoin(token: token)
                    continuation.resume(returning: res)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
        await refreshMeshes()
        return result
    }
    
    func getMeshInvite(meshId: String) async throws -> String {
        guard let lattice else { throw LatticeServiceError.notInitialized }
        let latticeInstance = lattice
        return try await withCheckedThrowingContinuation { continuation in
            ffiQueue.async {
                do {
                    let res = try latticeInstance.meshInvite(meshId: meshId)
                    continuation.resume(returning: res)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }
    
    func getMeshPeers(meshId: String) async throws -> [PeerInfo] {
        guard let lattice else { throw LatticeServiceError.notInitialized }
        let latticeInstance = lattice
        return try await withCheckedThrowingContinuation { continuation in
            ffiQueue.async {
                do {
                    let res = try latticeInstance.meshPeers(meshId: meshId)
                    continuation.resume(returning: res)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }
    
    // MARK: - Stores
    
    func refreshStores(mesh: MeshInfo) async {
        do {
            let list = try await getStores(mesh: mesh)
            stores[mesh.idString] = list
        } catch {
            print("[Lattice] Failed to refresh stores for \(mesh.idString): \(error)")
        }
    }
    
    func getStores(mesh: MeshInfo) async throws -> [StoreInfo] {
        guard let lattice else { throw LatticeServiceError.notInitialized }
        let latticeInstance = lattice
        let meshIdHex = mesh.idString
        let meshIdBytes = mesh.id
        
        return try await withCheckedThrowingContinuation { continuation in
            ffiQueue.async {
                do {
                    let declaredStores = try latticeInstance.storeList(meshId: meshIdHex)
                    
                    // Prepend root store with mesh ID bytes
                    let rootStore = StoreInfo(id: meshIdBytes, name: "Root", storeType: "kv", archived: false, details: nil)
                    var result = [rootStore]
                    result.append(contentsOf: declaredStores)
                    
                    continuation.resume(returning: result)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }
    
    func createStore(meshId: String, name: String?, storeType: String) async throws -> StoreInfo {
        guard let lattice else { throw LatticeServiceError.notInitialized }
        let latticeInstance = lattice
        return try await withCheckedThrowingContinuation { continuation in
            ffiQueue.async {
                do {
                    let res = try latticeInstance.storeCreate(meshId: meshId, name: name, storeType: storeType)
                    continuation.resume(returning: res)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }
    
    func getStoreStatus(storeId: String) async throws -> StoreInfo {
        guard let lattice else { throw LatticeServiceError.notInitialized }
        let latticeInstance = lattice
        return try await withCheckedThrowingContinuation { continuation in
            ffiQueue.async {
                do {
                    let res = try latticeInstance.storeStatus(storeId: storeId)
                    continuation.resume(returning: res)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }
    
    func syncStore(storeId: String) async throws {
        guard let lattice else { throw LatticeServiceError.notInitialized }
        let latticeInstance = lattice
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            ffiQueue.async {
                do {
                    try latticeInstance.storeSync(storeId: storeId)
                    continuation.resume()
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }
    
    func getStoreHistory(storeId: String) async throws -> [HistoryEntry] {
        guard let lattice else { throw LatticeServiceError.notInitialized }
        let latticeInstance = lattice
        return try await withCheckedThrowingContinuation { continuation in
            ffiQueue.async {
                do {
                    let res = try latticeInstance.storeHistory(storeId: storeId)
                    continuation.resume(returning: res)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }
    
    // MARK: - Reflection
    
    func inspectStore(storeId: String) async throws -> [MethodInfo] {
        guard let lattice else { throw LatticeServiceError.notInitialized }
        let latticeInstance = lattice
        return try await withCheckedThrowingContinuation { continuation in
            ffiQueue.async {
                do {
                    let res = try latticeInstance.storeInspect(storeId: storeId)
                    continuation.resume(returning: res)
                } catch {
                     continuation.resume(throwing: error)
                }
            }
        }
    }
    
    func executeStoreMethod(storeId: String, method: String, args: [ArgValue]) async throws -> ReflectValue {
        guard let lattice else { throw LatticeServiceError.notInitialized }
        let latticeInstance = lattice
        return try await withCheckedThrowingContinuation { continuation in
            ffiQueue.async {
                do {
                    let res = try latticeInstance.storeExecDynamic(storeId: storeId, methodName: method, args: args)
                    continuation.resume(returning: res)
                } catch {
                     continuation.resume(throwing: error)
                }
            }
        }
    }
    
    func getDescriptor(storeId: String) async throws -> DescriptorInfo {
        guard let lattice else { throw LatticeServiceError.notInitialized }
        return try await withCheckedThrowingContinuation { continuation in
            ffiQueue.async {
                do {
                    let info = try lattice.getStoreDescriptor(storeId: storeId)
                    continuation.resume(returning: info)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }
    
    func inspectType(storeId: String, typeName: String) async throws -> [FieldInfo] {
        guard let lattice else { throw LatticeServiceError.notInitialized }
        return try await withCheckedThrowingContinuation { continuation in
            ffiQueue.async {
                do {
                    let fields = try lattice.storeInspectType(storeId: storeId, typeName: typeName)
                    continuation.resume(returning: fields)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }
    
    // MARK: - Streams
    
    func listStreams(storeId: String) async throws -> [StreamInfo] {
        guard let lattice else { throw LatticeServiceError.notInitialized }
        return try await withCheckedThrowingContinuation { continuation in
            ffiQueue.async {
                do {
                    let streams = try lattice.storeListStreams(storeId: storeId)
                    continuation.resume(returning: streams)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }
    
    func subscribeStream(storeId: String, streamName: String, args: [ArgValue]) async throws -> StoreStream {
        guard let lattice else { throw LatticeServiceError.notInitialized }
        return try await withCheckedThrowingContinuation { continuation in
            ffiQueue.async {
                do {
                    let stream = try lattice.storeSubscribe(storeId: storeId, streamName: streamName, args: args)
                    continuation.resume(returning: stream)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }

    // MARK: - Events

    
    private func startEventListener() {
        guard let latticeInstance = lattice else { return }
        
        eventTask = Task {
            await runEventLoop(lattice: latticeInstance)
        }
    }
    
    private nonisolated func runEventLoop(lattice: Lattice) async {
        do {
            // Subscribe is now async in Rust bindings (and non-blocking)
            let stream = try await lattice.subscribe()
            
            while !Task.isCancelled {
                // native async next() call
                if let event = await stream.next() {
                    await self.handleEvent(event)
                } else {
                    break
                }
            }
        } catch {
            await MainActor.run {
                self.errorMessage = "Event stream connection failed: \(error.localizedDescription)"
            }
        }
    }
    
    private func handleEvent(_ event: BackendEvent) async {
        // Note: This method is @MainActor (inherited from class), so all
        // @Published property mutations are safe here.
        switch event {
        case .meshReady(let meshId):
            print("Mesh ready: \(meshId)")
            await refreshMeshes() // This is async, calls ffiQueue
        case .storeReady(let meshId, let storeId):
            print("Store ready: \(storeId) in mesh \(meshId)")
        case .joinFailed(let meshId, let reason):
            // Safe: we're on MainActor
            errorMessage = "Join failed for \(meshId): \(reason)"
        case .syncResult(let storeId, let peersSynced, let entriesSent, let entriesReceived):
            print("Sync: \(storeId) - \(peersSynced) peers, sent \(entriesSent), received \(entriesReceived)")
        }
    }
    
}

enum LatticeServiceError: LocalizedError {
    case notInitialized
    
    var errorDescription: String? {
        switch self {
        case .notInitialized:
            return "Lattice service not initialized"
        }
    }
}
