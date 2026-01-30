import SwiftUI
#if os(iOS)
import UIKit
#endif

struct MethodExecutionView: View {
    @EnvironmentObject var latticeService: LatticeService
    let storeId: String
    let method: MethodInfo
    var allMethods: [MethodInfo] = []
    var prefilledArgs: [String: ReflectValue] = [:]
    
    @State private var arguments: [String: String] = [:]
    @State private var boolArguments: [String: Bool] = [:]
    @State private var executionResult: ReflectValue?
    @State private var isExecuting = false
    @State private var errorMsg: String?
    @State private var selectedRowAction: RowAction?
    
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                // Arguments Group
                if !method.inputFields.isEmpty {
                    GroupBox(label: Text("Arguments")) {
                        VStack(alignment: .leading, spacing: 12) {
                            ForEach(method.inputFields, id: \.name) { field in
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(field.name)
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                    inputView(for: field)
                                }
                            }
                        }
                        .padding(8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                
                // Execute Button
                Button {
                    Task { await execute() }
                } label: {
                    if isExecuting {
                        ProgressView().controlSize(.small)
                    } else {
                        Text("Execute Command")
                            .frame(maxWidth: .infinity)
                    }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(isExecuting)
                
                // Results
                if let result = executionResult {
                    GroupBox(label: Text("Result")) {
                        ReflectValueView(
                            value: result,
                            methods: allMethods,
                            onRowAction: { action in
                                selectedRowAction = action
                            }
                        )
                        .padding(8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                
                // Error
                if let msg = errorMsg {
                    GroupBox(label: Text("Error").foregroundStyle(.red)) {
                        Text(msg)
                            .foregroundStyle(.red)
                            .padding(8)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
            .padding()
        }
        .navigationTitle(method.name)
        .sheet(item: $selectedRowAction) { action in
            NavigationStack {
                MethodExecutionView(
                    storeId: storeId,
                    method: action.method,
                    allMethods: allMethods,
                    prefilledArgs: action.argMapping
                )
                .environmentObject(latticeService)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel") { selectedRowAction = nil }
                    }
                }
            }
        }
        .onAppear {
            // Pre-fill args if provided
            for (name, value) in prefilledArgs {
                if let str = ActionMatcher.stringValue(from: value) {
                    arguments[name] = str
                }
            }
        }
    }
    
    @ViewBuilder
    private func inputView(for field: FieldInfo) -> some View {
        switch field.fieldType {
        case .bool:
            Toggle(isOn: Binding(
                get: { boolArguments[field.name, default: false] },
                set: { boolArguments[field.name] = $0 }
            )) {
                Text("Enabled")
            }
        default:
            textFieldBody(for: field)
        }
    }

    @ViewBuilder
    private func textFieldBody(for field: FieldInfo) -> some View {
        TextField(fieldTypeName(field), text: Binding(
            get: { arguments[field.name, default: "" ] },
            set: { arguments[field.name] = $0 }
        ))
        .autocorrectionDisabled()
        .textFieldStyle(.roundedBorder)
        
        #if os(iOS)
        .keyboardType(keyboardType(for: field.fieldType))
        #endif
    }
    
    private func fieldTypeName(_ field: FieldInfo) -> String {
        switch field.fieldType {
        case .string: return "string"
        case .int: return "int64"
        case .uint: return "uint64"
        case .float: return "float"
        case .bool: return "bool"
        case .bytes: return "bytes"
        case .message(let typeName): return typeName
        case .enum(let typeName): return typeName
        }
    }
    
    #if os(iOS)
    private func keyboardType(for type: FieldType) -> UIKeyboardType {
        switch type {
        case .int, .float: return .decimalPad
        default: return .default
        }
    }
    #endif
    
    private func execute() async {
        isExecuting = true
        errorMsg = nil
        executionResult = nil
        defer { isExecuting = false }
        
        print("[DEBUG] execute() - arguments: \(arguments)")
        print("[DEBUG] execute() - boolArguments: \(boolArguments)")
        
        do {
            let args = try buildArgs()
            // Detailed bytes debug
            for (i, arg) in args.enumerated() {
                if case .bytes(let data) = arg {
                    let hex = data.map { String(format: "%02x", $0) }.joined()
                    print("[DEBUG] arg[\(i)] bytes hex: \(hex)")
                }
            }
            print("[DEBUG] execute() - built args: \(args)")
            let result = try await latticeService.executeStoreMethod(storeId: storeId, method: method.name, args: args)
            print("[DEBUG] execute() - result: \(result)")
            executionResult = result
        } catch {
            print("[DEBUG] execute() - error: \(error)")
            errorMsg = error.localizedDescription
        }
    }
    
    private func buildArgs() throws -> [ArgValue] {
        var values: [ArgValue] = []
        
        // Arguments must be ORDERED according to inputFields
        for field in method.inputFields {
            let val = try parseValue(field: field)
            values.append(val)
        }
        
        return values
    }
    
    private func parseValue(field: FieldInfo) throws -> ArgValue {
        return try ArgValueParser.parse(
            field: field,
            stringValue: arguments[field.name],
            boolValue: boolArguments[field.name]
        )
    }
}
