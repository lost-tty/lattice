// ============================================================================
// helpers.js — UUID and hex utilities
// ============================================================================

const Helpers = (() => {
  function uuidFromBytes(u8) {
    if (!u8 || u8.length !== 16) return '??';
    const hex = Array.from(u8).map(b => b.toString(16).padStart(2, '0')).join('');
    return `${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`;
  }

  function uuidToBytes(uuid) {
    const hex = uuid.replace(/-/g, '');
    const bytes = new Uint8Array(16);
    for (let i = 0; i < 16; i++) bytes[i] = parseInt(hex.substr(i*2, 2), 16);
    return bytes;
  }

  function hexFromBytes(u8) {
    if (!u8 || u8.length === 0) return '';
    return Array.from(u8).map(b => b.toString(16).padStart(2, '0')).join('');
  }

  function bytesFromHex(hex) {
    return new Uint8Array(hex.match(/.{1,2}/g).map(h => parseInt(h, 16)));
  }

  function pubkeyShort(u8) {
    if (!u8) return '??';
    return hexFromBytes(u8);
  }

  return { uuidFromBytes, uuidToBytes, hexFromBytes, bytesFromHex, pubkeyShort };
})();
