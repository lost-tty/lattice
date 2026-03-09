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

// Render a proto SExpr (from GetIntentionResponse.ops) as a colored Preact vnode.
// SExpr uses oneof "value" with variants: symbol, str, raw, num, list.
// protobufjs decode() sets defaults on all fields, so we must check the
// oneof discriminator (sexpr.value) to find the active variant.
// Colors match the CLI scheme: symbol=blue, str=green, raw=magenta, num=yellow, parens=dimmed.
function fmtSExpr(sexpr) {
  if (!sexpr) return '';
  const which = sexpr.value; // oneof discriminator: "symbol"|"str"|"raw"|"num"|"list"
  switch (which) {
    case 'symbol': return html`<span class="sexpr-sym">${sexpr.symbol}</span>`;
    case 'str': return html`<span class="sexpr-str">"${sexpr.str}"</span>`;
    case 'raw': return html`<span class="sexpr-raw">${hex(sexpr.raw)}</span>`;
    case 'num': return html`<span class="sexpr-num">${String(sexpr.num)}</span>`;
    case 'list': {
      const items = (sexpr.list.items || []).map((item, i) =>
        i > 0 ? html`${' '}${fmtSExpr(item)}` : fmtSExpr(item)
      );
      return html`<span class="sexpr-paren">(</span>${items}<span class="sexpr-paren">)</span>`;
    }
    default: return '';
  }
}

// Render an array of SExpr ops as Preact vnodes, one per line.
function fmtOps(ops) {
  if (!ops || ops.length === 0) return '-';
  return ops.map((op, i) =>
    html`${i > 0 ? '\n' : ''}${fmtSExpr(op)}`
  );
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
