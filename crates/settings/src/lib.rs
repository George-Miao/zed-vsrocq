//! Typed VsRocq settings shared by the Zed extension and native bridge.
//!
//! Parse each flat settings layer, then merge the layers in priority order.
//! Known fields retain their presence and any typed parse failure until
//! validation. A higher-priority field can replace an invalid lower-priority
//! field. Only unknown fields use recursive raw JSON merging.
//!
//! Fallible settings operations return [`Error`]. Serde trait implementations
//! use the error type required by the serializer or deserializer.

mod error;
mod field;
mod raw;
mod settings;

pub use error::{Error, Result};
pub use field::Field;
pub use settings::{
    Bridge, Command, Completion, CompletionAlgorithm, Cursor, Delegation, Diagnostics, Diff, DiffMode, Display, Goals,
    Interrupt, Memory, Messages, PointInterpretation, PrettyPrint, Proof, ProofMode, ServerSettings, Settings,
};
