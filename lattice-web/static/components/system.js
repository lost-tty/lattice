// Decode a system table value based on known key patterns.
// Mirrors the CLI's decode_value() in store_commands.rs.
function decodeSystemValue(key, value) {
  if (!value || value.length === 0) return '';
  const hexStr = hex(value);

  try {
    // "name" → protobuf string (tag 1)
    if (key === 'name') {
      const decoded = decodeTag1String(value);
      if (decoded != null) return decoded + ' (0x' + hexStr + ')';
    }

    // "*/status" → peer status enum (tag 1 varint)
    if (key.endsWith('/status')) {
      const v = decodeTag1Varint(value);
      if (v != null) {
        const names = { 0: 'Unknown', 1: 'Invited', 2: 'Active', 3: 'Dormant', 4: 'Revoked' };
        const name = names[v];
        if (name) return name + ' (0x' + hexStr + ')';
      }
    }

    // "*/added_at" → timestamp uint64 (tag 1 varint)
    if (key.endsWith('/added_at')) {
      const v = decodeTag1Varint(value);
      if (v != null) return fmtTime(v) + ' (0x' + hexStr + ')';
    }

    // "*/added_by" → public key bytes (tag 1 bytes)
    if (key.endsWith('/added_by')) {
      const v = decodeTag1Bytes(value);
      if (v != null) return hex(v);
    }

    // "child/*" or "*/name" → raw UTF-8
    if (key.startsWith('child/') || key.endsWith('/name')) {
      const text = new TextDecoder().decode(value);
      if (/^[\x20-\x7e]*$/.test(text)) return text + ' (0x' + hexStr + ')';
    }

    // "strategy" → peer strategy oneof
    if (key === 'strategy') {
      const decoded = decodeStrategy(value);
      if (decoded) return decoded + ' (0x' + hexStr + ')';
    }
  } catch (e) { /* fall through to hex */ }

  return '0x' + hexStr;
}

// Decode a protobuf message with a single string field at tag 1.
function decodeTag1String(buf) {
  const r = protobuf.Reader.create(buf);
  while (r.pos < r.len) {
    const tag = r.uint32();
    if ((tag >>> 3) === 1 && (tag & 7) === 2) return r.string();
    r.skipType(tag & 7);
  }
  return null;
}

// Decode a protobuf message with a single varint field at tag 1.
function decodeTag1Varint(buf) {
  const r = protobuf.Reader.create(buf);
  while (r.pos < r.len) {
    const tag = r.uint32();
    if ((tag >>> 3) === 1 && (tag & 7) === 0) return r.uint64();
    r.skipType(tag & 7);
  }
  return null;
}

// Decode a protobuf message with a single bytes field at tag 1.
function decodeTag1Bytes(buf) {
  const r = protobuf.Reader.create(buf);
  while (r.pos < r.len) {
    const tag = r.uint32();
    if ((tag >>> 3) === 1 && (tag & 7) === 2) return r.bytes();
    r.skipType(tag & 7);
  }
  return null;
}

// Decode the PeerStrategy oneof (tags 1=Independent, 2=Inherited, 3=Snapshot).
function decodeStrategy(buf) {
  const r = protobuf.Reader.create(buf);
  while (r.pos < r.len) {
    const tag = r.uint32();
    const field = tag >>> 3;
    const wire = tag & 7;
    if (field === 1 && wire === 0) { r.bool(); return 'Independent'; }
    if (field === 2 && wire === 0) { r.bool(); return 'Inherited'; }
    if (field === 3 && wire === 2) {
      const id = r.bytes();
      const uuid = id.length === 16 ? Helpers.uuidFromBytes(id) : hex(id);
      return 'Snapshot(' + uuid + ')';
    }
    r.skipType(wire);
  }
  return null;
}

async function loadSystem(storeId) {
  const entries = (await API.store.SystemList({ id: storeId })).entries || [];
  if (entries.length === 0) {
    return html`<div class="empty-state">No system entries</div>`;
  }
  return html`
    <table>
      <tr><th>Key</th><th>Value</th></tr>
      ${entries.map(e => html`
        <tr>
          <td class="mono">${e.key}</td>
          <td class="mono">${decodeSystemValue(e.key, e.value)}</td>
        </tr>
      `)}
    </table>
  `;
}
