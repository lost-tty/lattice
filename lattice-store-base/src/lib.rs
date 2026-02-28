//! Base traits and infrastructure for Lattice store handles
//!
//! This crate provides:
//! - `Introspectable`, `CommandDispatcher`, `StreamReflectable` - reflection traits
//! - `StateProvider` - trait for handles to provide access to inner state
//! - Blanket impls that delegate reflection traits through `StateProvider`

pub mod dispatch;
mod handle;
mod introspection;
pub mod invoke;

pub use introspection::{
    BoxByteStream, CommandDispatcher, FieldFormat, Introspectable, StreamDescriptor, StreamError,
    StreamReflectable,
};

pub use handle::{CommandHandler, StateProvider, StreamHandler, StreamProvider, Subscriber};
pub use invoke::invoke_command;
