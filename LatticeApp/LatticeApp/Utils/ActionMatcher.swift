import Foundation

/// Represents a row action that can be triggered from table row context menu
struct RowAction: Identifiable {
    let id = UUID()
    let method: MethodInfo
    let argMapping: [String: ReflectValue]  // method arg name -> value from row
    
    var displayName: String {
        method.name.replacingOccurrences(of: "_", with: " ").capitalized
    }
}

/// Matches table row data to available methods using exact field name matching
struct ActionMatcher {
    
    /// Find methods whose inputs can be satisfied by row fields (exact name match)
    static func findRowActions(
        methods: [MethodInfo],
        rowFields: [ReflectField]
    ) -> [RowAction] {
        var actions: [RowAction] = []
        
        // Build field name -> value map
        let fieldMap = Dictionary(uniqueKeysWithValues: rowFields.map { ($0.name, $0.value) })
        
        for method in methods {
            // Skip methods with no inputs
            guard !method.inputFields.isEmpty else { continue }
            
            // Try to map each input field to a row field by exact name match
            var argMapping: [String: ReflectValue] = [:]
            var allMapped = true
            
            for inputField in method.inputFields {
                if let value = fieldMap[inputField.name] {
                    argMapping[inputField.name] = value
                } else {
                    allMapped = false
                    break
                }
            }
            
            // Only include if ALL args can be mapped
            if allMapped {
                actions.append(RowAction(method: method, argMapping: argMapping))
            }
        }
        
        return actions
    }
    
    /// Extract string value from ReflectValue for use as argument
    static func stringValue(from value: ReflectValue) -> String? {
        switch value {
        case .string(let s): return s
        case .int(let i): return String(i)
        case .uint(let u): return String(u)
        case .bytes(let data):
            if let str = String(data: data, encoding: .utf8) {
                return str
            }
            return data.map { String(format: "%02x", $0) }.joined()
        default: return nil
        }
    }
}
