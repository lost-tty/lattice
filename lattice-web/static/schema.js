// schema.js — dynamic store schema loading via protobufjs
//
// Fetches the binary FileDescriptorSet from /proto/store/{uuid}
// and converts it client-side via protobuf.Root.fromDescriptor().
// Uses `pb` (protobufjs) and `sdk` from sdk.js.

import { uuidFromBytes, bytesFromHex, hexFromBytes, displayBytes } from './helpers.js';
import { sdk, pb } from './sdk.js';

// Cache: storeUuid -> { root, serviceName, service }
const cache = new Map();

export async function getSchema(storeIdBytes) {
  const uuid = uuidFromBytes(storeIdBytes);
  if (cache.has(uuid)) return cache.get(uuid);

  const desc = await sdk.api.dynamic.GetDescriptor({ store_id: storeIdBytes });
  if (!desc.file_descriptor_set || desc.file_descriptor_set.length === 0) return null;
  const serviceName = desc.service_name || '';

  const resp = await fetch(`/proto/store/${uuid}`);
  if (!resp.ok) return null;
  const buf = await resp.arrayBuffer();

  const root = pb.Root.fromDescriptor(new Uint8Array(buf));
  const service = serviceName ? root.lookupService(serviceName) : null;

  const schema = { root, service, serviceName };
  cache.set(uuid, schema);
  return schema;
}

export function encodeMessage(msgType, values) {
  const obj = {};
  for (const field of msgType.fieldsArray) {
    const val = values[field.name];
    if (val === undefined || val === null || val === '') continue;
    obj[field.name] = coerceFieldValue(field, val);
  }
  const msg = msgType.create(obj);
  return msgType.encode(msg).finish();
}

function coerceFieldValue(field, val) {
  switch (field.type) {
    case 'string':
      return String(val);
    case 'bytes': {
      if (typeof val === 'string' && val.startsWith('0x')) {
        return bytesFromHex(val.slice(2));
      }
      return new TextEncoder().encode(val);
    }
    case 'bool':
      return val === 'true' || val === '1' || val === true;
    case 'double': case 'float':
      return parseFloat(val) || 0;
    default:
      return parseInt(val, 10) || 0;
  }
}

export function decodeMessage(msgType, bytes) {
  if (!bytes || bytes.length === 0) return {};
  return msgType.toObject(msgType.decode(bytes), {
    longs: Number,
    bytes: Uint8Array,
    defaults: true,
  });
}

export function formatFields(msgType, obj, depth) {
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
  const resolved = field.resolve();

  if (resolved.resolvedType) {
    if (resolved.resolvedType instanceof pb.Enum) {
      const enumObj = resolved.resolvedType;
      const name = Object.entries(enumObj.values).find(([, v]) => v === val);
      return name ? name[0] : String(val);
    }
    if (resolved.resolvedType instanceof pb.Type && depth < 6) {
      if (typeof val === 'object' && val !== null) {
        return formatFields(resolved.resolvedType, val, depth + 1);
      }
    }
  }

  if (field.type === 'bytes') {
    if (val instanceof Uint8Array) return displayBytes(val) || hexFromBytes(val);
    return String(val);
  }

  return String(val);
}

export function formatDecoded(fields, indent) {
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

export function fieldTypeName(field) {
  const resolved = field.resolve();
  if (resolved.resolvedType) return resolved.resolvedType.name;
  return field.type;
}

export function describeFields(root, typeName) {
  try {
    const t = root.lookupType(typeName);
    if (t && t.fieldsArray.length > 0)
      return t.fieldsArray.map(f => ({ name: f.name, typeName: fieldTypeName(f) }));
  } catch (e) { /* type not found */ }
  return null;
}
