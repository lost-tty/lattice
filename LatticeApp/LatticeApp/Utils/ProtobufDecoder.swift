import Foundation

enum WireType: Int {
    case varint = 0
    case fixed64 = 1
    case lengthDelimited = 2
    case startGroup = 3 // Deprecated
    case endGroup = 4   // Deprecated
    case fixed32 = 5
}

struct ProtoField: Identifiable {
    let id = UUID()
    let fieldNumber: Int
    let wireType: WireType
    let value: Any // UInt64, Data, UInt32
    
    // Helper to view content
    var asString: String? {
        if let data = value as? Data {
            return String(data: data, encoding: .utf8)
        }
        return nil
    }
    
    var asData: Data? {
        return value as? Data
    }
    
    var asVarint: UInt64? {
        return value as? UInt64
    }
}

class ProtobufDecoder {
    static func decode(_ data: Data) -> [ProtoField] {
        var fields: [ProtoField] = []
        var cursor = 0
        
        while cursor < data.count {
            // Read Key
            guard let (key, next) = readVarint(data, at: cursor) else { break }
            cursor = next
            
            let wireTypeVal = Int(key & 0x07)
            let fieldNum = Int(key >> 3)
            
            guard let wireType = WireType(rawValue: wireTypeVal) else {
                // Unknown wire type - we can't know how to skip it, so return what we have
                print("Unknown wire type \(wireTypeVal) at field \(fieldNum), stopping decode")
                return fields 
            }
            
            switch wireType {
            case .varint:
                guard let (val, next) = readVarint(data, at: cursor) else { return fields }
                fields.append(ProtoField(fieldNumber: fieldNum, wireType: wireType, value: val))
                cursor = next
                
            case .fixed64:
                if cursor + 8 <= data.count {
                    let val = data.subdata(in: cursor..<cursor+8)
                    fields.append(ProtoField(fieldNumber: fieldNum, wireType: wireType, value: val))
                    cursor += 8
                } else { return fields }
                
            case .lengthDelimited:
                guard let (len, next) = readVarint(data, at: cursor) else { return fields }
                cursor = next
                let length = Int(len)
                if cursor + length <= data.count {
                    let val = data.subdata(in: cursor..<cursor+length)
                    fields.append(ProtoField(fieldNumber: fieldNum, wireType: wireType, value: val))
                    cursor += length
                } else { return fields }
                
            case .fixed32:
                if cursor + 4 <= data.count {
                    let val = data.subdata(in: cursor..<cursor+4)
                    fields.append(ProtoField(fieldNumber: fieldNum, wireType: wireType, value: val))
                    cursor += 4
                } else { return fields }
                
            case .startGroup, .endGroup:
                // Deprecated wire types - we cannot safely skip group content
                // Return what we have to avoid corruption
                print("Deprecated group wire type at field \(fieldNum), stopping decode")
                return fields
            }
        }
        
        return fields
    }
    
    static func readVarint(_ data: Data, at index: Int) -> (UInt64, Int)? {
        var value: UInt64 = 0
        var shift: UInt64 = 0
        var cursor = index
        
        while cursor < data.count {
            let byte = data[cursor]
            cursor += 1
            
            value |= UInt64(byte & 0x7F) << shift
            if (byte & 0x80) == 0 {
                return (value, cursor)
            }
            shift += 7
            if shift >= 64 { return nil } // Overflow/Malform
        }
        return nil
    }
}
