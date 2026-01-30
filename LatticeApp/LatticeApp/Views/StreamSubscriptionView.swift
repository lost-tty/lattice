import SwiftUI

/// View for subscribing to and displaying store stream events
struct StreamSubscriptionView: View {
    @Environment(\.dismiss) private var dismiss
    @EnvironmentObject var latticeService: LatticeService
    let storeId: String
    
    @State private var streams: [StreamInfo] = []
    @State private var selectedStreamName: String = ""
    @State private var isLoading = false
    @State private var errorMsg: String?
    
    // Stream state
    @State private var isSubscribed = false
    @State private var events: [StreamEvent] = []
    @State private var streamTask: Task<Void, Never>?
    
    // Arguments input
    @State private var argInput = ""
    
    private var selectedStream: StreamInfo? {
        streams.first { $0.name == selectedStreamName }
    }
    
    var body: some View {
        VStack(spacing: 0) {
            // Header: Stream selector + controls
            VStack(spacing: 12) {
                HStack {
                    // Stream picker
                    Picker("Stream", selection: $selectedStreamName) {
                        Text("Select...").tag("")
                        ForEach(streams, id: \.name) { stream in
                            Text(stream.name).tag(stream.name)
                        }
                    }
                    .pickerStyle(.menu)
                    .disabled(isSubscribed)
                    .onChange(of: selectedStreamName) {
                        argInput = ""
                    }
                    
                    Spacer()
                    
                    // Subscribe/Stop button
                    Button {
                        if isSubscribed {
                            stopSubscription()
                        } else {
                            startSubscription()
                        }
                    } label: {
                        Label(
                            isSubscribed ? "Stop" : "Subscribe",
                            systemImage: isSubscribed ? "stop.circle.fill" : "play.circle.fill"
                        )
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(isSubscribed ? .red : .green)
                    .disabled(selectedStreamName.isEmpty)
                }
                
                // Parameter input
                if let stream = selectedStream {
                    HStack {
                        Text(stream.paramSchema ?? "Argument:")
                            .foregroundStyle(.secondary)
                            .font(.caption)
                        TextField("Value", text: $argInput)
                            .textFieldStyle(.roundedBorder)
                            .disabled(isSubscribed)
                    }
                }
                
                // Debug counter
                if !events.isEmpty {
                    Text("Events: \(events.count)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .padding()
            .background(.regularMaterial)
            
            Divider()
            
            // Event log
            if events.isEmpty && !isSubscribed {
                ContentUnavailableView(
                    "No Events",
                    systemImage: "waveform.path",
                    description: Text("Subscribe to a stream to see real-time events.")
                )
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 0) {
                            ForEach(events) { event in
                                VStack(alignment: .leading, spacing: 4) {
                                    HStack {
                                        Text(event.timestamp, style: .time)
                                            .font(.caption)
                                            .foregroundStyle(.secondary)
                                        Spacer()
                                        if event.isError {
                                            Image(systemName: "exclamationmark.triangle.fill")
                                                .foregroundStyle(.red)
                                        }
                                    }
                                    
                                    // Fallback text if reflection fails to render anything visible
                                    let v = event.value
                                    if case .null = v {
                                        Text("<null event>")
                                            .italic()
                                            .foregroundStyle(.secondary)
                                    } else {
                                        ReflectValueView(value: v)
                                    }
                                }
                                .padding(.horizontal)
                                .padding(.vertical, 8)
                                .id(event.id)
                                
                                Divider()
                            }
                        }
                    }
                    .onChange(of: events.count) {
                        // Auto-scroll to latest
                        if let last = events.last {
                            withAnimation {
                                proxy.scrollTo(last.id, anchor: .bottom)
                            }
                        }
                    }
                }
            }
        }
        .navigationTitle("Streams")
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Done") {
                    dismiss()
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Button {
                    events.removeAll()
                } label: {
                    Label("Clear", systemImage: "trash")
                }
                .disabled(events.isEmpty)
            }
        }
        .task {
            await loadStreams()
        }
        .onDisappear {
            stopSubscription()
        }
    }
    
    private func loadStreams() async {
        isLoading = true
        defer { isLoading = false }
        do {
            streams = try await latticeService.listStreams(storeId: storeId)
            // Auto-select first if available
            if selectedStreamName.isEmpty, let first = streams.first {
                selectedStreamName = first.name
            }
        } catch {
            errorMsg = error.localizedDescription
        }
    }
    
    private func startSubscription() {
        guard !selectedStreamName.isEmpty else { return }
        
        isSubscribed = true
        
        // Capture values to safely pass to task
        let sId = storeId
        let sName = selectedStreamName
        let argVal = argInput
        
        streamTask = Task { @MainActor in
            await runSubscriptionLoop(storeId: sId, streamName: sName, argVal: argVal)
        }
    }
    
    @MainActor
    private func runSubscriptionLoop(storeId: String, streamName: String, argVal: String) async {
        do {
            let args: [ArgValue] = argVal.isEmpty ? [] : [.string(argVal)]
            
            let stream = try await latticeService.subscribeStream(
                storeId: storeId,
                streamName: streamName,
                args: args
            )
            
            while !Task.isCancelled {
                do {
                    if let val = try await stream.next() {
                         events.append(StreamEvent(value: val, isError: false))
                    } else {
                         return // Stream ended
                    }
                } catch {
                    events.append(StreamEvent(
                        value: .string("Error: \(error.localizedDescription)"),
                        isError: true
                    ))
                }
            }
        } catch {
            if !Task.isCancelled {
                events.append(StreamEvent(
                    value: .string("Subscription error: \(error.localizedDescription)"),
                    isError: true
                ))
                isSubscribed = false
            }
        }
    }
    
    private func stopSubscription() {
        streamTask?.cancel()
        streamTask = nil
        isSubscribed = false
    }
}

// Helper to break MainActor isolation for blocking call
// MARK: - Event Model

struct StreamEvent: Identifiable {
    let id = UUID()
    let timestamp = Date()
    let value: ReflectValue
    let isError: Bool
}
