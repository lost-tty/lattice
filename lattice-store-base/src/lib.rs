//! Base traits and infrastructure for Lattice store handles
//!
//! This crate provides:
//! - `Introspectable`, `CommandDispatcher`, `StreamReflectable` - reflection traits
//! - `HandleBase<S, W>` - shared handle struct with common boilerplate
//! - `StateProvider` - trait for handles to provide access to inner state
//! - Blanket impls that delegate reflection traits through `StateProvider`

mod introspection;
mod handle;

pub use introspection::{
    BoxByteStream, CommandDispatcher, FieldFormat, Introspectable, StreamDescriptor,
    StreamError, StreamReflectable,
};

pub use handle::{HandleBase, StateProvider, Dispatchable, StreamHandler, StreamProvider};
