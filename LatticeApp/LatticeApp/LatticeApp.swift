import SwiftUI

@main
struct LatticeAppApp: App {
    @StateObject private var service: LatticeService
    @StateObject private var logs: LogCapture

    init() {
        // Install before LatticeService boots so its FFI runtime's tracing /
        // stdout output is captured from the first byte.
        LogCapture.shared.install()
        _logs = StateObject(wrappedValue: LogCapture.shared)
        _service = StateObject(wrappedValue: LatticeService.shared)
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(service)
                .environmentObject(logs)
                #if os(macOS)
                .frame(minWidth: 800, minHeight: 600)
                #endif
        }
        #if os(macOS)
        .commands {
            CommandGroup(replacing: .newItem) {}
        }
        #endif
    }
}
