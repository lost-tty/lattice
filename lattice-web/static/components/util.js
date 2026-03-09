// Shared helpers and Preact imports for all components.
// Every component file uses these globals.

const { html, Fragment, useState, useEffect, useRef } = P;

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
// Returns a Preact vnode (or null).
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

  return html`
    <table>
      <tr>${headers.map(h => html`<th>${h}</th>`)}</tr>
      ${rows.map(row => html`
        <tr>${row.map(f => html`<td class="mono">${fmtFieldValue(f)}</td>`)}</tr>
      `)}
    </table>
    <div class="muted table-count">${rows.length} item${rows.length !== 1 ? 's' : ''}</div>
  `;
}

// Build SExpr-like objects for fmtSExpr (mirrors protobufjs oneof shape).
function sexprSym(s)    { return { value: 'symbol', symbol: s }; }
function sexprList(items) { return { value: 'list', list: { items } }; }

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

// Pretty-print an SExpr with indentation. Lists containing sub-lists get
// each child on its own line. Rendered inside a white-space:pre-wrap container.
// Returns vnodes with \n and space characters for structure.
function fmtSExprPretty(sexpr, depth) {
  if (!sexpr) return '';
  depth = depth || 0;
  if (sexpr.value !== 'list') return fmtSExpr(sexpr);
  const items = sexpr.list.items || [];
  if (items.length === 0) return html`<span class="sexpr-paren">()</span>`;
  const hasSubLists = items.length > 2 && items.some(i => i.value === 'list');
  if (!hasSubLists) return fmtSExpr(sexpr);
  const indent = '  '.repeat(depth + 1);
  return html`<span class="sexpr-paren">(</span>${fmtSExpr(items[0])}${items.slice(1).map(child =>
    html`${'\n' + indent}${fmtSExprPretty(child, depth + 1)}`)}<span class="sexpr-paren">)</span>`;
}

// Render an array of SExpr ops as Preact vnodes, one per line.
function fmtOps(ops) {
  if (!ops || ops.length === 0) return '-';
  return ops.map((op, i) =>
    html`${i > 0 ? '\n' : ''}${fmtSExpr(op)}`
  );
}

// Extract unique intention hashes from witness log entries.
// Returns [{ hex, bytes }] — deduped, order-preserving.
function extractIntentionHashes(entries) {
  const seen = new Set();
  const result = [];
  for (const w of entries) {
    if (!w.content || w.content.length === 0) continue;
    try {
      const wc = T.WitnessContent.decode(w.content);
      if (wc.intention_hash && wc.intention_hash.length > 0) {
        const h = hex(wc.intention_hash);
        if (!seen.has(h)) { seen.add(h); result.push({ hex: h, bytes: wc.intention_hash }); }
      }
    } catch (e) { /* skip */ }
  }
  return result;
}

// Collect form field values from [data-field] elements.
function collectFormFields(formRef) {
  const values = {};
  if (formRef.current) {
    for (const el of formRef.current.querySelectorAll('[data-field]')) {
      const val = el.value;
      if (val !== '' && val !== undefined) values[el.dataset.field] = val;
    }
  }
  return values;
}

// Reusable form field input for protobufjs message types.
function FieldInput({ field, cssClass, autofocus }) {
  const resolved = field.resolve();
  const typeName = Schema.fieldTypeName(field);

  if (resolved.resolvedType instanceof protobuf.Enum) {
    return html`
      <label>${field.name} <span class="field-type">(${typeName})</span></label>
      <select class=${cssClass} data-field=${field.name} autofocus=${autofocus}>
        ${Object.entries(resolved.resolvedType.values).map(([name, num]) =>
          html`<option value=${num}>${name}</option>`
        )}
      </select>
    `;
  }

  if (field.type === 'bool') {
    return html`
      <label>${field.name} <span class="field-type">(${typeName})</span></label>
      <select class=${cssClass} data-field=${field.name} autofocus=${autofocus}>
        <option value="">false</option>
        <option value="true">true</option>
      </select>
    `;
  }

  const placeholder = field.type === 'bytes' ? 'text (or 0x... for hex)' : typeName;
  return html`
    <label>${field.name} <span class="field-type">(${typeName})</span></label>
    <input class=${cssClass} data-field=${field.name} placeholder=${placeholder} autofocus=${autofocus} />
  `;
}
