// ESM shim — re-exports the UMD globals set by preact-standalone.js and signals.min.js
export const { h, html, render, Fragment, Component, createContext, createRef,
  useState, useEffect, useRef, useMemo, useCallback, useReducer,
  useContext, useLayoutEffect } = P;
export const { signal, computed, effect, batch, untracked } = preactSignals;
