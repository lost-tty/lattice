import SwiftUI

struct LogView: View {
    @EnvironmentObject var logs: LogCapture
    /// True while the user is within `tailThreshold` points of the bottom.
    /// New log lines only auto-scroll while this holds — so manual scrolling
    /// up to read history isn't interrupted by incoming output.
    @State private var tailing = true

    /// Bottom padding within which we consider the user to be "at the bottom".
    private static let tailThreshold: CGFloat = 40

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(logs.lines.indices, id: \.self) { i in
                        Text(logs.lines[i])
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.horizontal, 8)
                            .padding(.vertical, 1)
                            .id(i)
                    }
                    Color.clear.frame(height: 1).id("bottom")
                }
            }
            .background(Color.black.opacity(0.02))
            .onScrollGeometryChange(for: Bool.self) { geom in
                let distanceFromBottom = geom.contentSize.height
                    - (geom.contentOffset.y + geom.containerSize.height)
                return distanceFromBottom <= Self.tailThreshold
            } action: { _, nowTailing in
                tailing = nowTailing
            }
            .onChange(of: logs.lines.count) { _, _ in
                guard tailing else { return }
                proxy.scrollTo("bottom", anchor: .bottom)
            }
            .onAppear {
                proxy.scrollTo("bottom", anchor: .bottom)
            }
        }
    }
}
