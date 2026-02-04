//! Base traits and infrastructure for Lattice store handles
//!
//! This crate provides:
//! - `Introspectable`, `CommandDispatcher`, `StreamReflectable` - reflection traits
//! - `StateProvider` - trait for handles to provide access to inner state
//! - Blanket impls that delegate reflection traits through `StateProvider`

mod introspection;
mod handle;
pub mod dispatch;
pub mod invoke;

pub use introspection::{
    BoxByteStream, CommandDispatcher, FieldFormat, Introspectable, StreamDescriptor,
    StreamError, StreamReflectable,
};

pub use handle::{StateProvider, Dispatchable, Dispatcher, StreamHandler, StreamProvider, HandleWithWriter};
pub use invoke::invoke_command;
