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
    @Published var apps: [AppBindingProto] = []

    private var lattice: Lattice?
    private let ffiQueue = DispatchQueue(label: "com.lattice.ffi", qos: .default)

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
            await refreshApps()
        } catch {
            state = .failed(error.localizedDescription)
        }
    }

    // MARK: - Apps

    func refreshApps() async {
        guard let lattice else { return }
        let instance = lattice
        do {
            let list = try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<[AppBindingProto], Error>) in
                ffiQueue.async {
                    do {
                        let apps = try instance.listApps()
                        continuation.resume(returning: apps)
                    } catch {
                        continuation.resume(throwing: error)
                    }
                }
            }
            self.apps = list.sorted(by: { $0.subdomain < $1.subdomain })
        } catch {
            // Apps may not be ready yet during startup
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
