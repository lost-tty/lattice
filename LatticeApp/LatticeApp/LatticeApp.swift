import SwiftUI

@main
struct LatticeAppApp: App {
    @StateObject private var latticeService = LatticeService.shared
    
    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(latticeService)
        }
        #if os(macOS)
        .commands {
            CommandGroup(replacing: .newItem) { }
        }
        #endif
    }
}
