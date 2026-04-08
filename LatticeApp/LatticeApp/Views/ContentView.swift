import SwiftUI

/// A tab identity — either the main node UI or an app subdomain.
enum Tab: Hashable {
    case node
    case app(String) // subdomain
}

struct ContentView: View {
    @EnvironmentObject var service: LatticeService
    @State private var selectedTab: Tab = .node

    var body: some View {
        switch service.state {
        case .stopped, .starting:
            VStack(spacing: 16) {
                ProgressView()
                    .controlSize(.large)
                Text("Starting Lattice...")
                    .foregroundStyle(.secondary)
            }
        case .running(let url):
            tabView(baseURL: url)
        case .failed(let error):
            VStack(spacing: 16) {
                Image(systemName: "exclamationmark.triangle")
                    .font(.system(size: 48))
                    .foregroundStyle(.red)
                Text("Failed to Start")
                    .font(.title2)
                Text(error)
                    .font(.caption)
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal)
                Button("Retry") {
                    Task { await service.start() }
                }
                .buttonStyle(.borderedProminent)
            }
            .padding()
        }
    }

    private func tabView(baseURL: String) -> some View {
        VStack(spacing: 0) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 0) {
                    tabButton("Node", systemImage: "network", tab: .node)
                    ForEach(service.apps, id: \.subdomain) { app in
                        tabButton(app.subdomain, systemImage: "app", tab: .app(app.subdomain))
                    }
                }
                .padding(.horizontal, 8)
            }
            .frame(height: 36)

            Divider()

            ZStack {
                WebView(
                    url: URL(string: baseURL)!,
                    onNavigate: handleNavigation
                )
                .opacity(selectedTab == .node ? 1 : 0)

                ForEach(service.apps, id: \.subdomain) { app in
                    if let url = service.appURL(subdomain: app.subdomain) {
                        WebView(
                            url: url,
                            onNavigate: handleNavigation
                        )
                        .opacity(selectedTab == .app(app.subdomain) ? 1 : 0)
                    }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .ignoresSafeArea(edges: .bottom)
    }

    private func tabButton(_ title: String, systemImage: String, tab: Tab) -> some View {
        Button {
            selectedTab = tab
        } label: {
            Label(title, systemImage: systemImage)
                .font(.callout)
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(selectedTab == tab ? Color.accentColor.opacity(0.15) : Color.clear)
                .cornerRadius(6)
        }
        .buttonStyle(.plain)
    }

    private func handleNavigation(_ url: URL) -> Bool {
        guard let host = url.host else { return true }
        let suffix = ".localhost"
        guard host.hasSuffix(suffix) else { return true }

        let subdomain = String(host.dropLast(suffix.count))
        if subdomain.isEmpty { return true }

        if service.apps.contains(where: { $0.subdomain == subdomain }) {
            selectedTab = .app(subdomain)
            return false
        }

        return true
    }
}
