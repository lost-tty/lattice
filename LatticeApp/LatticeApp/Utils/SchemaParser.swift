import Foundation

struct FieldDef {
    let name: String
    let number: Int
    let typeName: String? // For messages/enums
    let type: Int // Type enum value from descriptor.proto
}

struct MessageDef {
    let name: String
    let fullName: String
    let fields: [Int: FieldDef] // Map field number to definition
}

struct ServiceDef {
    let name: String
    let fullName: String
    let methods: [String: MethodDef] // Map name to definition
}

struct MethodDef {
    let name: String
    let inputType: String
    let outputType: String
}

class SchemaRegistry {
    private(set) var messages: [String: MessageDef] = [:] // FullName -> Def
    private(set) var services: [String: ServiceDef] = [:] // FullName -> Def
    
    // Parse FileDescriptorSet bytes
    func parse(_ data: Data) {
        let rootFields = ProtobufDecoder.decode(data)
        // Root should have repeated field 1 (file)
        for field in rootFields where field.fieldNumber == 1 && field.wireType == .lengthDelimited {
            if let fileData = field.asData {
                parseFile(fileData)
            }
        }
    }
    
    private func parseFile(_ data: Data) {
        let fields = ProtobufDecoder.decode(data)
        var package = ""
        
        // 1. Get Package (Field 2)
        for f in fields where f.fieldNumber == 2 {
            if let s = f.asString { package = s }
        }
        
        let prefix = package.isEmpty ? "" : ".\(package)"
        
        // 2. Parse Messages (Field 4)
        for f in fields where f.fieldNumber == 4 {
            if let d = f.asData {
                parseMessage(d, parent: prefix)
            }
        }
        
        // 3. Parse Services (Field 6)
        for f in fields where f.fieldNumber == 6 {
            if let d = f.asData {
                parseService(d, parent: prefix)
            }
        }
    }
    
    private func parseMessage(_ data: Data, parent: String) {
        let fields = ProtobufDecoder.decode(data)
        var name = ""
        
        // Name (1)
        for f in fields where f.fieldNumber == 1 {
            if let s = f.asString { name = s }
        }
        
        let fullName = "\(parent).\(name)"
        var fieldDefs: [Int: FieldDef] = [:]
        
        // Fields (2)
        for f in fields where f.fieldNumber == 2 {
            if let d = f.asData {
                if let fd = parseField(d) {
                    fieldDefs[fd.number] = fd
                }
            }
        }
        
        let msgDef = MessageDef(name: name, fullName: fullName, fields: fieldDefs)
        messages[fullName] = msgDef
        
        // Nested Types (3)
        for f in fields where f.fieldNumber == 3 {
            if let d = f.asData {
                parseMessage(d, parent: fullName)
            }
        }
    }
    
    private func parseField(_ data: Data) -> FieldDef? {
        let fields = ProtobufDecoder.decode(data)
        var name = ""
        var number = 0
        var typeName: String?
        var type = 0
        
        for f in fields {
            switch f.fieldNumber {
            case 1: name = f.asString ?? ""
            case 3: number = Int(f.asVarint ?? 0)
            case 5: type = Int(f.asVarint ?? 0)
            case 6: typeName = f.asString
            default: break
            }
        }
        
        if number == 0 { return nil }
        return FieldDef(name: name, number: number, typeName: typeName, type: type)
    }
    
    private func parseService(_ data: Data, parent: String) {
        let fields = ProtobufDecoder.decode(data)
        var name = ""
        
        for f in fields where f.fieldNumber == 1 {
            if let s = f.asString { name = s }
        }
        
        let fullName = "\(parent).\(name)"
        var methods: [String: MethodDef] = [:]
        
        // Methods (2)
        for f in fields where f.fieldNumber == 2 {
            if let d = f.asData {
                if let md = parseMethod(d) {
                    methods[md.name] = md
                }
            }
        }
        
        services[fullName] = ServiceDef(name: name, fullName: fullName, methods: methods)
    }
    
    private func parseMethod(_ data: Data) -> MethodDef? {
        let fields = ProtobufDecoder.decode(data)
        var name = ""
        var input = ""
        var output = ""
        
        for f in fields {
            switch f.fieldNumber {
            case 1: name = f.asString ?? ""
            case 2: input = f.asString ?? ""
            case 3: output = f.asString ?? ""
            default: break
            }
        }
        
        if name.isEmpty { return nil }
        return MethodDef(name: name, inputType: input, outputType: output)
    }
}
