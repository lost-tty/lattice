// ============================================================================
// dag.js — DAG layout engine for intention history graphs
//
// Pure layout logic, no DOM/Preact dependency.
// Input:  Map<hexHash, { hash, author, hlc, causalDeps[], isMerge, ... }>
// Output: { rows[], edges[], maxCol, totalRows }
// ============================================================================

function layoutDag(entries) {
  const entryHashes = new Set(entries.keys());

  // Build children map
  const children = new Map();
  for (const [hash, entry] of entries) {
    for (const parent of entry.causalDeps) {
      if (entryHashes.has(parent)) {
        if (!children.has(parent)) children.set(parent, []);
        children.get(parent).push(hash);
      }
    }
  }
  for (const [, kids] of children) {
    kids.sort((a, b) => entries.get(a).hlc - entries.get(b).hlc);
  }

  // Kahn's algorithm with HLC priority
  const inDegree = new Map();
  for (const [hash, entry] of entries) {
    inDegree.set(hash, entry.causalDeps.filter(p => entryHashes.has(p)).length);
  }

  const ready = [];
  function pushReady(hash) {
    const hlc = entries.get(hash).hlc;
    let i = ready.length;
    while (i > 0 && ready[i - 1].hlc > hlc) i--;
    ready.splice(i, 0, { hash, hlc });
  }

  for (const [hash, degree] of inDegree) {
    if (degree === 0) pushReady(hash);
  }

  const order = [];
  while (ready.length > 0) {
    const { hash } = ready.shift();
    order.push(hash);
    const kids = children.get(hash);
    if (kids) {
      for (const child of kids) {
        const d = inDegree.get(child) - 1;
        inDegree.set(child, d);
        if (d === 0) pushReady(child);
      }
    }
  }

  const hashToRow = new Map();
  order.forEach((h, i) => hashToRow.set(h, i));

  // Last descendant row per entry
  const lastDescRow = new Map();
  for (const hash of order) {
    const myRow = hashToRow.get(hash);
    for (const parent of entries.get(hash).causalDeps) {
      if (entryHashes.has(parent)) {
        lastDescRow.set(parent, Math.max(lastDescRow.get(parent) || 0, myRow));
      }
    }
  }

  // Column assignment with reuse.
  // activeColumns: columns with a live vertical edge running through.
  // reservedUntil: col -> row until which a pre-reserved fork edge occupies it.
  const hashToCol = new Map();
  const activeColumns = new Set();
  const columnTips = new Map();
  const reservedUntil = new Map();
  let maxCol = 0;

  function colFree(col, currentRow) {
    if (activeColumns.has(col)) return false;
    const until = reservedUntil.get(col);
    if (until !== undefined && until >= currentRow) return false;
    return true;
  }

  function findFreeCol(currentRow) {
    for (let col = 0; col <= maxCol; col++) {
      if (colFree(col, currentRow)) return col;
    }
    return maxCol + 1;
  }

  // Pre-reserve columns for fork edges.
  // When a parent has N children, the first N-1 (non-inheriting) get new
  // columns reserved from the parent row to the child row.
  function reserveForksFrom(parentHash, parentRow) {
    const kids = children.get(parentHash);
    if (!kids || kids.length <= 1) return;
    for (let i = 0; i < kids.length - 1; i++) {
      const childRow = hashToRow.get(kids[i]);
      const col = findFreeCol(parentRow);
      maxCol = Math.max(maxCol, col);
      hashToCol.set(kids[i], col);
      reservedUntil.set(col, childRow);
    }
  }

  for (let row = 0; row < order.length; row++) {
    const hash = order[row];
    const entry = entries.get(hash);
    const parentsInSet = entry.causalDeps.filter(p => entryHashes.has(p));

    // If column was pre-assigned by a fork reservation, use it
    let col = hashToCol.get(hash);
    if (col === undefined) {
      if (parentsInSet.length === 0) {
        col = findFreeCol(row);
        maxCol = Math.max(maxCol, col);
      } else if (parentsInSet.length > 1) {
        col = Math.min(...parentsInSet.map(p => hashToCol.get(p)).filter(c => c !== undefined));
        if (col === Infinity) col = findFreeCol(row);
      } else {
        const parent = parentsInSet[0];
        const parentCol = hashToCol.get(parent);
        if (parentCol === undefined) {
          col = findFreeCol(row);
          maxCol = Math.max(maxCol, col);
        } else {
          const isBlocked = !colFree(parentCol, row) && columnTips.get(parentCol) !== parent;
          if (isBlocked) {
            col = findFreeCol(row);
            maxCol = Math.max(maxCol, col);
          } else {
            col = parentCol;
          }
        }
      }
      hashToCol.set(hash, col);
    }

    // Clear reservation now that the node has been placed
    if (reservedUntil.get(col) === row) {
      reservedUntil.delete(col);
    }

    // Mark current node's column as active if it has future descendants
    if ((lastDescRow.get(hash) || 0) > row) {
      activeColumns.add(col);
      columnTips.set(col, hash);
    } else {
      activeColumns.delete(col);
      columnTips.delete(col);
    }

    // Free parent columns that terminate at this row
    for (const parent of parentsInSet) {
      if (lastDescRow.get(parent) === row) {
        const pCol = hashToCol.get(parent);
        if (pCol !== col || (lastDescRow.get(hash) || 0) <= row) {
          activeColumns.delete(pCol);
          columnTips.delete(pCol);
        }
      }
    }

    // Pre-reserve columns for fork children
    reserveForksFrom(hash, row);
  }

  // Build layout rows + collect all edges
  const edges = [];
  const rows = order.map(hash => {
    const entry = entries.get(hash);
    const row = hashToRow.get(hash);
    const col = hashToCol.get(hash);
    const parentsInSet = entry.causalDeps.filter(p => entryHashes.has(p));

    for (const parentHash of parentsInSet) {
      edges.push({
        parentRow: hashToRow.get(parentHash),
        parentCol: hashToCol.get(parentHash),
        childRow: row,
        childCol: col,
        isMerge: entry.isMerge,
      });
    }

    return { hash, entry, row, col, isRoot: parentsInSet.length === 0 };
  });

  return { rows, edges, maxCol, totalRows: order.length };
}
