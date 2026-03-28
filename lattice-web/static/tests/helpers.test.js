// Helpers tests — run via boa_engine in cargo test

import {
  uuidFromBytes, uuidToBytes, hexFromBytes, bytesFromHex,
  displayBytes, isRootStore, getRootStores,
} from '../helpers.js';

const tests = [];
function test(name, fn) { tests.push({ name, fn }); }
function assert(cond, msg) { if (!cond) throw new Error(msg || 'assertion failed'); }
function eq(a, b, msg) { assert(a === b, (msg || '') + ' expected ' + JSON.stringify(b) + ' got ' + JSON.stringify(a)); }

// --- UUID ---

test('uuid round-trip', () => {
  const uuid = 'a1b2c3d4-e5f6-7890-abcd-ef0123456789';
  eq(uuidFromBytes(uuidToBytes(uuid)), uuid);
});

test('uuidFromBytes(null) → ??', () => {
  eq(uuidFromBytes(null), '??');
});

test('uuidFromBytes(empty) → ??', () => {
  eq(uuidFromBytes(new Uint8Array(0)), '??');
});

test('uuidToBytes returns 16 bytes', () => {
  eq(uuidToBytes('a1b2c3d4-e5f6-7890-abcd-ef0123456789').length, 16);
});

// --- Hex ---

test('hex round-trip', () => {
  eq(hexFromBytes(bytesFromHex('deadbeef')), 'deadbeef');
});

test('hexFromBytes', () => {
  eq(hexFromBytes(new Uint8Array([0xde, 0xad])), 'dead');
});

test('bytesFromHex', () => {
  const b = bytesFromHex('dead');
  eq(b[0], 222);
  eq(b[1], 173);
});

test('hexFromBytes(null) → empty', () => {
  eq(hexFromBytes(null), '');
});

test('bytesFromHex empty → empty array', () => {
  eq(bytesFromHex('').length, 0);
});

// --- displayBytes ---

test('displayBytes ascii', () => {
  eq(displayBytes(new TextEncoder().encode('hello')), 'hello');
});

test('displayBytes binary → hex', () => {
  eq(displayBytes(new Uint8Array([0xff, 0x00])), 'ff00');
});

test('displayBytes null → empty', () => {
  eq(displayBytes(null), '');
});

// --- store type ---

test('isRootStore true', () => {
  assert(isRootStore({ store_type: 'core:rootstore' }));
});

test('isRootStore false', () => {
  assert(!isRootStore({ store_type: 'core:kvstore' }));
});

test('getRootStores filters', () => {
  const stores = [{ store_type: 'core:rootstore' }, { store_type: 'core:kvstore' }];
  eq(getRootStores(stores).length, 1);
});

// --- run all ---

const results = { passed: 0, failed: 0, errors: [] };
for (const t of tests) {
  try {
    t.fn();
    results.passed++;
  } catch (e) {
    results.failed++;
    results.errors.push(t.name + ': ' + e.message);
  }
}
export default results;
