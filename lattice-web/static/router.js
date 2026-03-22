// Minimal path-based router for the management UI.
//
// URL scheme:
//   /                              dashboard
//   /store/{uuid}                  store view (details tab)
//   /store/{uuid}/{tab}            store view with specific tab
//   /store/{uuid}/debug/{view}     debug sub-view
//
// Uses history.pushState — no hash fragments.

const listeners = [];

/** Parse current location.pathname into a route object. */
export function parse(path) {
  const p = (path || location.pathname).replace(/\/+$/, '') || '/';
  const parts = p.split('/').filter(Boolean);

  if (parts[0] === 'store' && parts[1]) {
    const route = { page: 'store', storeId: parts[1], tab: 'details' };
    if (parts[2]) {
      route.tab = parts[2];
    }
    if (parts[2] === 'debug' && parts[3]) {
      route.debugView = parts[3];
    }
    return route;
  }

  return { page: 'dashboard' };
}

/** Build a path string from a route object. */
export function path(route) {
  if (!route || route.page === 'dashboard') return '/';
  if (route.page === 'store') {
    let p = '/store/' + route.storeId;
    if (route.tab && route.tab !== 'details') {
      p += '/' + route.tab;
    }
    if (route.tab === 'debug' && route.debugView && route.debugView !== 'tips') {
      p += '/' + route.debugView;
    }
    return p;
  }
  return '/';
}

/** Navigate to a new path. */
export function navigate(newPath, replace) {
  if (newPath === location.pathname) return;
  if (replace) {
    history.replaceState(null, '', newPath);
  } else {
    history.pushState(null, '', newPath);
  }
  notify();
}

/** Subscribe to route changes. Returns unsubscribe function. */
export function listen(fn) {
  listeners.push(fn);
  return () => {
    const i = listeners.indexOf(fn);
    if (i >= 0) listeners.splice(i, 1);
  };
}

function notify() {
  const route = parse();
  for (const fn of listeners) fn(route);
}

// Handle browser back/forward
window.addEventListener('popstate', () => notify());
