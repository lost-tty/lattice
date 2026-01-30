import Foundation

enum ArgValueParser {
    static func parse(field: FieldInfo, stringValue: String?, boolValue: Bool?) throws -> ArgValue {
        let fieldType = field.fieldType
        
        switch fieldType {
        case .bool:
            return ArgValue.bool(boolValue ?? false)
            
        case .string:
            return ArgValue.string(stringValue ?? "")
            
        case .int:
            let raw = stringValue ?? ""
            if let i = Int64(raw) {
                return ArgValue.int(i)
            }
            if raw.isEmpty {
                return ArgValue.null
            }
            throw NSError(domain: "Lattice", code: 1, userInfo: [NSLocalizedDescriptionKey: "Invalid integer for \(field.name)"])
            
        case .uint:
            let raw = stringValue ?? ""
            if let u = UInt64(raw) {
                return ArgValue.int(Int64(bitPattern: u))
            }
            if raw.isEmpty {
                return ArgValue.null
            }
            throw NSError(domain: "Lattice", code: 1, userInfo: [NSLocalizedDescriptionKey: "Invalid unsigned integer for \(field.name)"])
            
        case .float:
            let raw = stringValue ?? ""
            if let f = Double(raw) {
                return ArgValue.float(f)
            }
            if raw.isEmpty {
                return ArgValue.null
            }
            throw NSError(domain: "Lattice", code: 1, userInfo: [NSLocalizedDescriptionKey: "Invalid float for \(field.name)"])
            
        case .bytes:
            let data = (stringValue ?? "").data(using: .utf8) ?? Data()
            return ArgValue.bytes(data)
            
        case .message, .enum:
            // For nested message or enum, pass as string (user provides value)
            return ArgValue.string(stringValue ?? "")
        }
    }
}
