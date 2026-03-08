// Shared helpers and Preact imports for all components.
// Every component file uses these globals.

const { html, Fragment, useState, useEffect, useRef, useMemo, useCallback } = P;

function hex(bytes) {
  return bytes ? Helpers.hexFromBytes(bytes) : '';
}

function fmtTime(ms) {
  return ms ? new Date(Number(ms)).toLocaleString() : '-';
}

function fmtFieldValue(fld) {
  if (Array.isArray(fld.value)) {
    return fld.value.map(v =>
      Array.isArray(v) ? Schema.formatDecoded(v) : String(v)
    ).join(', ');
  }
  return String(fld.value);
}

// Detect tabular data: a single repeated message field where all items
// share the same field names. Mirrors the CLI's table detection.
function tryRenderTable(fields) {
  function findRepeatedMessages(fs) {
    if (fs.length === 1 && fs[0].type === 'repeated' && Array.isArray(fs[0].value)) {
      const items = fs[0].value;
      if (items.length > 0 && items.every(item => Array.isArray(item))) return items;
    }
    if (fs.length === 1 && Array.isArray(fs[0].value) && fs[0].type !== 'repeated') {
      return findRepeatedMessages(fs[0].value);
    }
    return null;
  }

  const rows = findRepeatedMessages(fields);
  if (!rows || rows.length === 0) return null;

  const headers = rows[0].map(f => f.name);
  for (const row of rows) {
    if (row.length !== headers.length) return null;
    for (let i = 0; i < headers.length; i++) {
      if (row[i].name !== headers[i]) return null;
    }
  }

  const ths = headers.map(h => `<th>${h}</th>`).join('');
  const trs = rows.map(row => {
    const tds = row.map(f => `<td class="mono">${fmtFieldValue(f)}</td>`).join('');
    return `<tr>${tds}</tr>`;
  }).join('');

  return `<table><tr>${ths}</tr>${trs}</table>
    <div class="muted" style="margin-top:8px">${rows.length} item${rows.length !== 1 ? 's' : ''}</div>`;
}

// Reusable form field input for protobufjs message types.
function FieldInput({ field, cssClass, autofocus }) {
  const resolved = field.resolve();
  const typeName = Schema.fieldTypeName(field);

  if (resolved.resolvedType instanceof protobuf.Enum) {
    return html`
      <label>${field.name} <span style="color:var(--muted)">(${typeName})</span></label>
      <select class=${cssClass} data-field=${field.name} autofocus=${autofocus}>
        ${Object.entries(resolved.resolvedType.values).map(([name, num]) =>
          html`<option value=${num}>${name}</option>`
        )}
      </select>
    `;
  }

  if (field.type === 'bool') {
    return html`
      <label>${field.name} <span style="color:var(--muted)">(${typeName})</span></label>
      <select class=${cssClass} data-field=${field.name} autofocus=${autofocus}>
        <option value="">false</option>
        <option value="true">true</option>
      </select>
    `;
  }

  const placeholder = field.type === 'bytes' ? 'text (or 0x... for hex)' : typeName;
  return html`
    <label>${field.name} <span style="color:var(--muted)">(${typeName})</span></label>
    <input class=${cssClass} data-field=${field.name} placeholder=${placeholder} autofocus=${autofocus} />
  `;
}
