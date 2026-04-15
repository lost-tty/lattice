// helpers.js — UUID, hex, and display utilities

export function uuidFromBytes(u8) {
  if (!u8 || u8.length !== 16) return '??';
  const hex = Array.from(u8).map(b => b.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`;
}

export function uuidToBytes(uuid) {
  const hex = uuid.replace(/-/g, '');
  const bytes = new Uint8Array(16);
  for (let i = 0; i < 16; i++) bytes[i] = parseInt(hex.substring(i*2, i*2+2), 16);
  return bytes;
}

export function hexFromBytes(u8) {
  if (!u8 || u8.length === 0) return '';
  return Array.from(u8).map(b => b.toString(16).padStart(2, '0')).join('');
}

export function bytesFromHex(hex) {
  if (!hex) return new Uint8Array(0);
  const m = hex.match(/.{1,2}/g);
  return m ? new Uint8Array(m.map(h => parseInt(h, 16))) : new Uint8Array(0);
}

export function pubkeyShort(u8) {
  if (!u8) return '??';
  return hexFromBytes(u8);
}

// Derive a unique oklch color from a hex string (pubkey, hash, etc).
export function colorFromHex(h) {
  let h1 = 0, h2 = 0;
  for (let i = 0; i < 16 && i < h.length; i += 2) {
    const b = parseInt(h.slice(i, i + 2), 16) || 0;
    h1 = (h1 * 31 + b) >>> 0;
    h2 = (h2 * 17 + b) >>> 0;
  }
  const hue = (h1 * 137.508) % 360;
  const tier = (h2 >>> 4) & 1;
  const L = tier ? 0.75 : 0.65;
  const C = tier ? 0.18 : 0.25;
  return `oklch(${L} ${C} ${hue.toFixed(1)})`;
}

// Display a Uint8Array as printable text (if ASCII) or hex.
export function displayBytes(u8) {
  if (!u8 || u8.length === 0) return '';
  const text = new TextDecoder().decode(u8);
  if (/^[\x20-\x7e\n\r\t]*$/.test(text) && text.length > 0) return text;
  return hexFromBytes(u8);
}

export function isRootStore(s) {
  return s.store_type === 'core:rootstore';
}

// Build the URL that opens the given app subdomain. Mirrors the current
// page's protocol: under `lattice://` we keep the scheme and use the
// subdomain as the host; under `http(s)://` we use the conventional
// `<sub>.<host>:<port>/` shape.
export function appHref(subdomain) {
  if (location.protocol === 'lattice:') {
    return 'lattice://' + subdomain + '/';
  }
  return location.protocol + '//' + subdomain + '.' + location.host + '/';
}

export function getRootStores(stores) {
  return (stores || []).filter(isRootStore);
}
