// Shared helpers and Preact imports for all components.
// Every component file can import from this module.

import { html, Fragment, useState, useEffect, useRef, useMemo } from '../vendor/preact.mjs';
import { hexFromBytes } from '../helpers.js';
import { fieldTypeName, formatDecoded } from '../schema.js';
import { sdk, pb } from '../sdk.js';

// Re-export Preact primitives so components can import from util.js.
export { html, Fragment, useState, useEffect, useRef, useMemo };

// ---------------------------------------------------------------------------
// Icon component — reusable SVG icons
// Usage: html`<${Icon} name="plus" />` or html`<${Icon} name="plus" size=${16} />`
// All icons use a 24-unit viewBox with stroke style, except "play" which is filled.
// ---------------------------------------------------------------------------

const ICONS = {
  plus: html`<line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/>`,
  close: html`<line x1="6" y1="6" x2="18" y2="18"/><line x1="18" y1="6" x2="6" y2="18"/>`,
  pause: html`<line x1="8" y1="6" x2="8" y2="18"/><line x1="16" y1="6" x2="16" y2="18"/>`,
  play: html`<polygon points="6,4 20,12 6,20"/>`,
  'log-in': html`<path d="M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4"/><polyline points="10 17 15 12 10 7"/><line x1="15" y1="12" x2="3" y2="12"/>`,
};

export function Icon({ name, size }) {
  const s = size || 14;
  const paths = ICONS[name];
  if (!paths) return null;
  const filled = name === 'play';
  return html`<svg width=${s} height=${s} viewBox="0 0 24 24"
    fill=${filled ? 'currentColor' : 'none'}
    stroke=${filled ? 'none' : 'currentColor'}
    stroke-width="2.5" stroke-linecap="round">${paths}</svg>`;
}

export function hex(bytes) {
  return bytes ? hexFromBytes(bytes) : '';
}

export function fmtTime(ms) {
  return ms ? new Date(Number(ms)).toLocaleString() : '-';
}

function fieldsToSExpr(fields) {
  const sym = s => ({ value: 'symbol', symbol: s });
  const str = s => ({ value: 'str', str: String(s) });
  const num = n => ({ value: 'num', num: n });
  const list = items => ({ value: 'list', list: { items } });

  const items = [];
  for (const f of fields) {
    items.push(sym(f.name));
    if (Array.isArray(f.value) && f.type === 'repeated') {
      items.push(list(f.value.map(v => Array.isArray(v) ? fieldsToSExpr(v) : str(v))));
    } else if (Array.isArray(f.value)) {
      items.push(fieldsToSExpr(f.value));
    } else {
      items.push(typeof f.value === 'number' ? num(f.value) : str(f.value));
    }
  }
  return list(items);
}

function ppSExpr(sexpr, depth) {
  if (!sexpr) return '';
  if (sexpr.value !== 'list') return fmtSExpr(sexpr);
  const items = sexpr.list.items || [];
  if (items.length === 0) return html`<span class="sexpr-paren">()</span>`;
  // Simple lists (no nested lists) render inline
  if (!items.some(i => i.value === 'list')) return fmtSExpr(sexpr);
  // Complex lists: each child on its own indented line
  const pad = '  '.repeat(depth + 1);
  return html`<span class="sexpr-paren">(</span>${items.map((child, i) =>
    html`${i > 0 ? '\n' + pad : ''}${ppSExpr(child, depth + 1)}`
  )}<span class="sexpr-paren">)</span>`;
}

export function fmtFieldValue(fld) {
  if (Array.isArray(fld.value)) {
    return html`<span class="sexpr-block">${ppSExpr(fieldsToSExpr(fld.value), 0)}</span>`;
  }
  return String(fld.value);
}

// Detect tabular data: responses that follow the convention have a field
// named 'items' (repeated message). Mirrors the CLI's table detection.
// Returns a Preact vnode (or null).
export function tryRenderTable(fields) {
  const itemsField = fields.find(f => f.name === 'items' && f.type === 'repeated');
  if (!itemsField || !Array.isArray(itemsField.value)) return null;
  const rows = itemsField.value;
  if (rows.length === 0 || !rows.every(item => Array.isArray(item))) return null;

  const headers = rows[0].map(f => f.name);
  for (const row of rows) {
    if (row.length !== headers.length) return null;
    for (let i = 0; i < headers.length; i++) {
      if (row[i].name !== headers[i]) return null;
    }
  }

  return html`
    <table class="data">
      <tr>${headers.map(h => html`<th>${h}</th>`)}</tr>
      ${rows.map(row => html`
        <tr>${row.map(f => html`<td class="mono">${fmtFieldValue(f)}</td>`)}</tr>
      `)}
    </table>
    <div class="muted table-count">${rows.length} item${rows.length !== 1 ? 's' : ''}</div>
  `;
}

// Render a proto SExpr (from GetIntentionResponse.ops) as a colored Preact vnode.
// SExpr uses oneof "value" with variants: symbol, str, raw, num, list.
// protobufjs decode() sets defaults on all fields, so we must check the
// oneof discriminator (sexpr.value) to find the active variant.
// Colors match the CLI scheme: symbol=blue, str=green, raw=magenta, num=yellow, parens=dimmed.
export function fmtSExpr(sexpr) {
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
export function fmtSExprPretty(sexpr, depth) {
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
export function fmtOps(ops) {
  if (!ops || ops.length === 0) return '-';
  return ops.map((op, i) =>
    html`${i > 0 ? '\n' : ''}${fmtSExpr(op)}`
  );
}

// Extract unique intention hashes from witness log entries.
// Returns [{ hex, bytes }] — deduped, order-preserving.
export function extractIntentionHashes(entries) {
  const WitnessContent = sdk.proto.lookup('lattice.weaver.WitnessContent');
  const seen = new Set();
  const result = [];
  for (const w of entries) {
    if (!w.content || w.content.length === 0) continue;
    const wc = WitnessContent.decode(w.content);
    if (wc.intention_hash && wc.intention_hash.length > 0) {
      const h = hex(wc.intention_hash);
      if (!seen.has(h)) { seen.add(h); result.push({ hex: h, bytes: wc.intention_hash }); }
    }
  }
  return result;
}

// Collect form field values from [data-field] elements.
export function collectFormFields(formRef) {
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
export function FieldInput({ field, cssClass, autofocus }) {
  const resolved = field.resolve();
  const typeName = fieldTypeName(field);

  if (resolved.resolvedType instanceof pb.Enum) {
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
