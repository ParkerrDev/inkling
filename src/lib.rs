//! inkling's core: fish-like inline suggestions for bash.
//!
//! The shell runs inside a pseudo-terminal that this program owns and passes
//! everything through untouched in both directions. The shell tells it where
//! it is with private marks that are taken out of the stream before the
//! terminal sees them, and asks readline itself, with a probe, what the line
//! is. See README.md for the whole design.
//!
//! None of this prints. It answers in values and errors, and writes bytes
//! only to the terminal and the pseudo-terminal it was given. The program in
//! `main.rs` is a thin face over it.

pub mod column;
pub mod error;
pub mod keys;
pub mod osc;
pub mod quote;
pub mod render;
pub mod text;

pub use error::{Error, Result};
