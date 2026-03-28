// Router tests — run via boa_engine in cargo test

import { parse, path } from '../router.js';

const tests = [];
function test(name, fn) { tests.push({ name, fn }); }
function assert(cond, msg) { if (!cond) throw new Error(msg || 'assertion failed'); }
function eq(a, b, msg) { assert(a === b, (msg || '') + ' expected ' + JSON.stringify(b) + ' got ' + JSON.stringify(a)); }

// --- parse ---

test('parse /', () => {
  eq(parse('/').page, 'dashboard');
});

test('parse /store/abc-123', () => {
  const r = parse('/store/abc-123');
  eq(r.page, 'store');
  eq(r.storeId, 'abc-123');
  eq(r.tab, 'details');
});

test('parse /store/abc-123/peers', () => {
  eq(parse('/store/abc-123/peers').tab, 'peers');
});

test('parse /store/abc-123/debug/log', () => {
  const r = parse('/store/abc-123/debug/log');
  eq(r.tab, 'debug');
  eq(r.debugView, 'log');
});

test('parse /store/abc-123/debug (no view)', () => {
  eq(parse('/store/abc-123/debug').tab, 'debug');
});

test('parse /unknown → dashboard', () => {
  eq(parse('/unknown').page, 'dashboard');
});

test('parse /store/ (no id) → dashboard', () => {
  eq(parse('/store/').page, 'dashboard');
});

test('parse trailing slash stripped', () => {
  eq(parse('/store/abc/peers/').tab, 'peers');
});

// --- path ---

test('path dashboard', () => {
  eq(path({ page: 'dashboard' }), '/');
});

test('path null', () => {
  eq(path(null), '/');
});

test('path store default tab omitted', () => {
  eq(path({ page: 'store', storeId: 'abc' }), '/store/abc');
  eq(path({ page: 'store', storeId: 'abc', tab: 'details' }), '/store/abc');
});

test('path store with tab', () => {
  eq(path({ page: 'store', storeId: 'abc', tab: 'peers' }), '/store/abc/peers');
});

test('path debug default view omitted', () => {
  eq(path({ page: 'store', storeId: 'abc', tab: 'debug', debugView: 'tips' }), '/store/abc/debug');
});

test('path debug with view', () => {
  eq(path({ page: 'store', storeId: 'abc', tab: 'debug', debugView: 'log' }), '/store/abc/debug/log');
});

// --- round-trips ---

test('round-trip /', () => eq(path(parse('/')), '/'));
test('round-trip /store/abc-123', () => eq(path(parse('/store/abc-123')), '/store/abc-123'));
test('round-trip /store/abc-123/peers', () => eq(path(parse('/store/abc-123/peers')), '/store/abc-123/peers'));
test('round-trip /store/abc-123/debug/log', () => eq(path(parse('/store/abc-123/debug/log')), '/store/abc-123/debug/log'));

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
