// ============================================================================
// Schema — dynamic store schema loading via protobufjs
//
// Fetches the binary FileDescriptorSet from /proto/store/{uuid}
// and converts it client-side via protobuf.Root.fromDescriptor().
// ============================================================================

const Schema = (() => {
  // Cache: storeUuid -> { root, serviceName, service }
  const cache = new Map();

  // Fetch and cache schema for a store.
  // Returns { root, service } where root is a protobuf.Root and
  // service is the protobufjs Service reflection object (with methods).
  async function getSchema(storeIdBytes) {
    const uuid = Helpers.uuidFromBytes(storeIdBytes);
    if (cache.has(uuid)) return cache.get(uuid);

    // Get service name via the existing RPC (we need it to look up the service)
    const desc = await API.dynamic.GetDescriptor({ id: storeIdBytes });
    if (!desc.file_descriptor_set || desc.file_descriptor_set.length === 0) return null;
    const serviceName = desc.service_name || '';

    // Fetch binary FileDescriptorSet and convert client-side
    const resp = await fetch(`/proto/store/${uuid}`);
    if (!resp.ok) return null;
    const buf = await resp.arrayBuffer();

    const root = protobuf.Root.fromDescriptor(new Uint8Array(buf));
    const service = serviceName ? root.lookupService(serviceName) : null;

    const schema = { root, service, serviceName };
    cache.set(uuid, schema);
    return schema;
  }

  function invalidate(storeIdBytes) {
    cache.delete(Helpers.uuidFromBytes(storeIdBytes));
  }

  // Encode a message from field name → string value map.
  // Uses the protobufjs type to handle proper encoding.
  function encodeMessage(msgType, values) {
    const obj = {};
    for (const field of msgType.fieldsArray) {
      const val = values[field.name];
      if (val === undefined || val === null || val === '') continue;
      obj[field.name] = coerceFieldValue(field, val);
    }
    const msg = msgType.create(obj);
    return msgType.encode(msg).finish();
  }

  // Coerce a string input value to the correct JS type for a protobufjs field.
  function coerceFieldValue(field, val) {
    switch (field.type) {
      case 'string':
        return String(val);
      case 'bytes': {
        if (typeof val === 'string' && val.startsWith('0x')) {
          return Helpers.bytesFromHex(val.slice(2));
        }
        return new TextEncoder().encode(val);
      }
      case 'bool':
        return val === 'true' || val === '1' || val === true;
      case 'double': case 'float':
        return parseFloat(val) || 0;
      default:
        // int32, uint32, int64, uint64, sint32, sint64, enum, etc.
        return parseInt(val, 10) || 0;
    }
  }

  // Decode response bytes using the output message type.
  // Returns a plain JS object with decoded fields.
  function decodeMessage(msgType, bytes) {
    if (!bytes || bytes.length === 0) return {};
    return msgType.toObject(msgType.decode(bytes), {
      longs: Number,
      bytes: Uint8Array,
      defaults: false,
    });
  }

  // Format a decoded object into display-friendly {name, value} array.
  // Walks the message type to produce labeled fields.
  function formatFields(msgType, obj, depth) {
    depth = depth || 0;
    const result = [];
    for (const field of msgType.fieldsArray) {
      let val = obj[field.name];
      if (val === undefined || val === null) continue;

      if (field.repeated) {
        if (!Array.isArray(val)) val = [val];
        const items = val.map(v => formatSingleValue(field, v, depth));
        result.push({ name: field.name, value: items, type: 'repeated' });
      } else {
        const formatted = formatSingleValue(field, val, depth);
        result.push({ name: field.name, value: formatted, type: field.type });
      }
    }
    return result;
  }

  function formatSingleValue(field, val, depth) {
    // Resolve the field's type
    const resolved = field.resolve();

    if (resolved.resolvedType) {
      if (resolved.resolvedType instanceof protobuf.Enum) {
        // Enum — look up value name
        const enumObj = resolved.resolvedType;
        const name = Object.entries(enumObj.values).find(([, v]) => v === val);
        return name ? name[0] : String(val);
      }
      if (resolved.resolvedType instanceof protobuf.Type && depth < 3) {
        // Nested message — recurse
        if (typeof val === 'object' && val !== null) {
          return formatFields(resolved.resolvedType, val, depth + 1);
        }
      }
    }

    // bytes
    if (field.type === 'bytes') {
      if (val instanceof Uint8Array) {
        const text = new TextDecoder().decode(val);
        if (/^[\x20-\x7e\n\r\t]*$/.test(text) && text.length > 0) return text;
        return Helpers.hexFromBytes(val);
      }
      return String(val);
    }

    return String(val);
  }

  // Format a decoded fields array as readable text.
  function formatDecoded(fields, indent) {
    indent = indent || 0;
    const pad = '  '.repeat(indent);
    const lines = [];
    for (const f of fields) {
      if (Array.isArray(f.value) && f.type === 'repeated') {
        lines.push(`${pad}${f.name}:`);
        for (const item of f.value) {
          if (Array.isArray(item)) {
            lines.push(`${pad}  -`);
            lines.push(formatDecoded(item, indent + 2));
          } else {
            lines.push(`${pad}  - ${item}`);
          }
        }
      } else if (Array.isArray(f.value)) {
        lines.push(`${pad}${f.name}:`);
        lines.push(formatDecoded(f.value, indent + 1));
      } else {
        lines.push(`${pad}${f.name}: ${f.value}`);
      }
    }
    return lines.join('\n');
  }

  // Get human-readable type name for a field.
  function fieldTypeName(field) {
    const resolved = field.resolve();
    if (resolved.resolvedType) return resolved.resolvedType.name;
    return field.type;
  }

  return { getSchema, invalidate, encodeMessage, decodeMessage,
           formatFields, formatDecoded, fieldTypeName, coerceFieldValue };
})();
