import SwiftUI

/// A tab identity — either the main node UI, an app subdomain, or the
/// in-app log viewer.
enum Tab: Hashable {
    case node
    case app(String) // subdomain
    case logs
}

struct ContentView: View {
    @EnvironmentObject var service: LatticeService
    @EnvironmentObject var logs: LogCapture
    @State private var selectedTab: Tab = .node
    @State private var reloadTokens: [Tab: Int] = [:]
    /// Tabs that have been visited at least once — webviews are only
    /// mounted for these, so unused tabs don't load in the background.
    @State private var activated: Set<Tab> = [.node]

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
            HStack(spacing: 0) {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 0) {
                        tabButton("Node", systemImage: "network", tab: .node, hasUpdate: false)
                        ForEach(service.apps) { app in
                            tabButton(
                                app.binding.subdomain,
                                systemImage: "app",
                                tab: .app(app.binding.subdomain),
                                hasUpdate: app.hasPendingUpdate
                            )
                        }
                        tabButton("Logs", systemImage: "doc.text", tab: .logs, hasUpdate: false)
                    }
                    .padding(.horizontal, 8)
                }

                trailingButton
                    .padding(.horizontal, 8)
            }

            Divider()

            ZStack {
                if activated.contains(.node) {
                    WebView(
                        url: URL(string: baseURL)!,
                        reloadToken: reloadTokens[.node, default: 0],
                        onNavigate: handleNavigation
                    )
                    .opacity(selectedTab == .node ? 1 : 0)
                }

                ForEach(service.apps) { app in
                    let sub = app.binding.subdomain
                    let tab = Tab.app(sub)
                    if activated.contains(tab), let url = service.appURL(subdomain: sub) {
                        WebView(
                            url: url,
                            reloadToken: reloadTokens[tab, default: 0],
                            onNavigate: handleNavigation
                        )
                        .opacity(selectedTab == tab ? 1 : 0)
                    }
                }

                if activated.contains(.logs) {
                    LogView()
                        .opacity(selectedTab == .logs ? 1 : 0)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .ignoresSafeArea(edges: .bottom)
    }

    /// Context-sensitive trailing button: reload for node/app tabs, clear for
    /// the logs tab.
    @ViewBuilder
    private var trailingButton: some View {
        switch selectedTab {
        case .logs:
            Button(action: logs.clear) {
                Image(systemName: "trash")
                    .imageScale(.large)
                    .foregroundStyle(Color.secondary)
                    .frame(minWidth: 44, minHeight: 44)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help("Clear logs")
        case .node, .app:
            let hasUpdate = selectedPendingUpdate
            Button(action: reloadSelected) {
                Image(systemName: hasUpdate ? "arrow.clockwise.circle.fill" : "arrow.clockwise")
                    .imageScale(.large)
                    .foregroundStyle(hasUpdate ? Color.accentColor : Color.secondary)
                    .frame(minWidth: 44, minHeight: 44)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(hasUpdate ? "New version available — reload" : "Reload")
        }
    }

    private var selectedPendingUpdate: Bool {
        guard case .app(let sub) = selectedTab else { return false }
        return service.apps.first(where: { $0.binding.subdomain == sub })?.hasPendingUpdate ?? false
    }

    private func reloadSelected() {
        reloadTokens[selectedTab, default: 0] &+= 1
        if case .app(let sub) = selectedTab {
            service.markReloaded(subdomain: sub)
        }
    }

    private func select(_ tab: Tab) {
        let firstActivation = activated.insert(tab).inserted
        selectedTab = tab
        // First time we show this tab's WebView, count it as a (re)load so the
        // "update available" badge has a baseline to compare against.
        if firstActivation, case .app(let sub) = tab {
            service.markReloaded(subdomain: sub)
        }
    }

    private func tabButton(_ title: String, systemImage: String, tab: Tab, hasUpdate: Bool) -> some View {
        Button {
            select(tab)
        } label: {
            HStack(spacing: 6) {
                Label(title, systemImage: systemImage)
                if hasUpdate {
                    Circle()
                        .fill(Color.accentColor)
                        .frame(width: 6, height: 6)
                }
            }
            .padding(.horizontal, 12)
            .frame(minHeight: 44)
            .background(selectedTab == tab ? Color.accentColor.opacity(0.15) : Color.clear)
            .cornerRadius(6)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private func handleNavigation(_ url: URL) -> Bool {
        guard let host = url.host else { return true }
        let suffix = ".localhost"
        guard host.hasSuffix(suffix) else { return true }

        let subdomain = String(host.dropLast(suffix.count))
        if subdomain.isEmpty { return true }

        if service.apps.contains(where: { $0.binding.subdomain == subdomain }) {
            select(.app(subdomain))
            return false
        }

        return true
    }
}
