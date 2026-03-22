// ESM shim for the SDK and protobufjs globals.
//
// LatticeSDK and protobuf are loaded as UMD scripts before any modules run.
// This module provides a single access point so no other module touches
// window globals directly.

/** The LatticeSDK constructor (from the UMD script). */
export const LatticeSDK = window.LatticeSDK;

/** The protobufjs library (from the UMD script). */
export const pb = window.protobuf;

/** @type {any} The connected SDK instance, set after connect(). */
export let sdk = null;

/** Set the SDK instance after connect(). */
export function setSdk(instance) {
  sdk = instance;
}
