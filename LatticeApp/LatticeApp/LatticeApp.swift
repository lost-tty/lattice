import SwiftUI

@main
struct LatticeAppApp: App {
    @StateObject private var service = LatticeService.shared

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(service)
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
