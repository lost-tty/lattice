import Foundation
import Combine

/// Redirects stdout + stderr into a ring buffer so the app can display its own
/// log output. Must be installed before anything writes to stdout/stderr.
///
/// Pipe reads happen on a background queue. Completed lines are staged under a
/// lock; a 100 ms main-thread timer flushes them into the `@Published` buffer
/// in one batch. This decouples log volume from SwiftUI invalidation rate and
/// keeps the main thread from being overwhelmed (which previously tripped the
/// iOS scene-update watchdog).
final class LogCapture: ObservableObject {
    static let shared = LogCapture()

    /// Max lines retained in the visible ring buffer.
    private static let capacity = 5000
    /// Max lines allowed in the staging buffer before we start dropping the
    /// oldest. If this fires we're producing faster than the UI can absorb.
    private static let stagingBackpressure = 20_000
    /// How often to flush staged lines onto the main-thread `@Published` array.
    private static let flushInterval: TimeInterval = 0.1

    @Published private(set) var lines: [String] = []
    /// Monotonic count of lines dropped due to backpressure.
    @Published private(set) var dropped: UInt64 = 0

    private var installed = false
    private var readSource: DispatchSourceRead?
    private var flushTimer: Timer?

    /// Background-queue-owned state.
    private let stagingLock = NSLock()
    private var staging: [String] = []
    private var partialLine = ""

    private init() {}

    func install() {
        guard !installed else { return }
        installed = true

        var fds: [Int32] = [0, 0]
        guard pipe(&fds) == 0 else { return }
        let readFd = fds[0]
        let writeFd = fds[1]

        setvbuf(stdout, nil, _IOLBF, 0)
        setvbuf(stderr, nil, _IONBF, 0)

        dup2(writeFd, fileno(stdout))
        dup2(writeFd, fileno(stderr))
        close(writeFd)

        let source = DispatchSource.makeReadSource(fileDescriptor: readFd, queue: .global(qos: .utility))
        source.setEventHandler { [weak self] in
            var buf = [UInt8](repeating: 0, count: 4096)
            let n = read(readFd, &buf, buf.count)
            guard n > 0 else { return }
            let chunk = String(decoding: buf.prefix(n), as: UTF8.self)
            self?.ingestBackground(chunk)
        }
        source.resume()
        self.readSource = source

        let timer = Timer(timeInterval: Self.flushInterval, repeats: true) { [weak self] _ in
            self?.flush()
        }
        RunLoop.main.add(timer, forMode: .common)
        self.flushTimer = timer
    }

    /// Called on the background read queue. No main-actor work — just parse
    /// and stage. Flushing to `@Published` happens on the timer.
    private func ingestBackground(_ chunk: String) {
        stagingLock.lock()
        partialLine += chunk
        while let newline = partialLine.firstIndex(of: "\n") {
            let line = String(partialLine[..<newline])
            partialLine.removeSubrange(...newline)
            staging.append(line)
        }
        // Backpressure: if staging gets too deep, drop oldest to cap memory.
        if staging.count > Self.stagingBackpressure {
            let excess = staging.count - Self.stagingBackpressure
            staging.removeFirst(excess)
            // Record drops — flushed on the next timer tick alongside lines.
            droppedPending &+= UInt64(excess)
        }
        stagingLock.unlock()
    }

    private var droppedPending: UInt64 = 0

    private func flush() {
        stagingLock.lock()
        let batch = staging
        let drops = droppedPending
        staging.removeAll(keepingCapacity: true)
        droppedPending = 0
        stagingLock.unlock()

        guard !batch.isEmpty || drops > 0 else { return }

        if drops > 0 { dropped &+= drops }
        if !batch.isEmpty {
            lines.append(contentsOf: batch)
            if lines.count > Self.capacity {
                lines.removeFirst(lines.count - Self.capacity)
            }
        }
    }

    func clear() {
        stagingLock.lock()
        staging.removeAll()
        partialLine = ""
        droppedPending = 0
        stagingLock.unlock()
        lines.removeAll()
        dropped = 0
    }
}
