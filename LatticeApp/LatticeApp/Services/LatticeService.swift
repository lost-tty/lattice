import Foundation
import Combine
#if os(iOS)
import UIKit
#endif

/// Manages the Lattice runtime via FFI bindings, including the embedded web server.
@MainActor
class LatticeService: ObservableObject {
    static let shared = LatticeService()

    enum State: Equatable {
        case stopped
        case starting
        case running(url: String)
        case failed(String)
    }

    @Published var state: State = .stopped
    @Published var apps: [AppState] = []

    private var lattice: Lattice?
    private let ffiQueue = DispatchQueue(label: "com.lattice.ffi", qos: .default)
    private var appStreamTask: Task<Void, Never>?

    /// Observable per-app UI state. The "update available" signal falls out of
    /// comparing `bundleUpdatedAt` (most recent `BundleChanged` for this
    /// app_id) with `lastReloadedAt` (most recent time this tab's content was
    /// (re)loaded). No explicit initial-vs-delta flag is needed: if the user
    /// has never loaded the tab, there's nothing to be "newer than", so no
    /// badge; once they load, any later bundle change is naturally pending.
    struct AppState: Identifiable {
        var binding: AppBindingProto
        var bundleUpdatedAt: Date?
        var lastReloadedAt: Date?

        var id: String { binding.subdomain }

        var hasPendingUpdate: Bool {
            guard let updated = bundleUpdatedAt, let reloaded = lastReloadedAt else {
                return false
            }
            return updated > reloaded
        }
    }

    var baseURL: String? {
        if case .running(let url) = state { return url }
        return nil
    }

    private init() {
        Task { await start() }
    }

    // MARK: - Lifecycle

    func start() async {
        guard case .stopped = state else { return }
        state = .starting

        do {
            let instance = await withCheckedContinuation { continuation in
                ffiQueue.async {
                    continuation.resume(returning: Lattice(dataDir: nil))
                }
            }
            self.lattice = instance

            var deviceName: String?
            #if os(iOS)
            deviceName = UIDevice.current.name
            #elseif os(macOS)
            deviceName = Host.current().localizedName
            #endif

            // Use the platform app support directory so it works in the iOS sandbox
            // and shares data with the CLI on macOS.
            let dataDir = Self.dataDirectory()
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                ffiQueue.async {
                    do {
                        try instance.start(dataDir: dataDir, name: deviceName, webPort: 0)
                        continuation.resume()
                    } catch {
                        continuation.resume(throwing: error)
                    }
                }
            }

            // The runtime reports the actual bound URL.
            let url = try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<String, Error>) in
                ffiQueue.async {
                    do {
                        guard let url = try instance.webUrl() else {
                            continuation.resume(throwing: LatticeServiceError.noWebURL)
                            return
                        }
                        continuation.resume(returning: url)
                    } catch {
                        continuation.resume(throwing: error)
                    }
                }
            }

            state = .running(url: url)
            startAppEventStream(instance: instance)
        } catch {
            state = .failed(error.localizedDescription)
        }
    }

    // MARK: - Apps

    /// Record that the user just (re)loaded the tab for this subdomain.
    /// Called on first tab activation and on explicit reload. Subsequent bundle
    /// updates compare their timestamp against this to decide if a badge shows.
    func markReloaded(subdomain: String) {
        guard let i = apps.firstIndex(where: { $0.binding.subdomain == subdomain }) else { return }
        apps[i].lastReloadedAt = .now
    }

    private func startAppEventStream(instance: Lattice) {
        appStreamTask?.cancel()
        appStreamTask = Task { [weak self] in
            let stream: AppEventStream
            do {
                stream = try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<AppEventStream, Error>) in
                    self?.ffiQueue.async {
                        do {
                            continuation.resume(returning: try instance.subscribeApps())
                        } catch {
                            continuation.resume(throwing: error)
                        }
                    }
                }
            } catch {
                return
            }

            while !Task.isCancelled {
                guard let event = await stream.next() else { break }
                await MainActor.run { self?.apply(event) }
            }
        }
    }

    private func apply(_ event: FfiAppEvent) {
        switch event {
        case .appAvailable(let binding):
            if let i = apps.firstIndex(where: { $0.binding.subdomain == binding.subdomain }) {
                apps[i].binding = binding
            } else {
                apps.append(AppState(binding: binding))
                apps.sort { $0.binding.subdomain < $1.binding.subdomain }
            }
        case .appRemoved(let subdomain):
            apps.removeAll { $0.binding.subdomain == subdomain }
        case .bundleChanged(let appId):
            if let i = apps.firstIndex(where: { $0.binding.appId == appId }) {
                apps[i].bundleUpdatedAt = .now
            }
        case .bundleRemoved:
            break
        }
    }

    /// URL for an app's subdomain webview.
    func appURL(subdomain: String) -> URL? {
        guard let base = baseURL, let parsed = URL(string: base), let port = parsed.port else {
            return nil
        }
        return URL(string: "http://\(subdomain).localhost:\(port)")
    }

    // MARK: - Private

    /// Returns the data directory path, or nil to use the Rust default.
    /// On macOS: nil (lets Rust dirs::data_dir() resolve to ~/Library/Application Support/lattice)
    /// On iOS: ~/Documents/LatticeData (original location, preserves existing data)
    private static func dataDirectory() -> String? {
        #if os(iOS)
        let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        let dir = docs.appendingPathComponent("LatticeData")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.path
        #else
        return nil
        #endif
    }

    enum LatticeServiceError: LocalizedError {
        case noWebURL
        var errorDescription: String? { "Web server did not report a URL" }
    }

}
