//! Byte Pair Encoding for KUTE.
//!
//! * [`BpeTokenizer`] (`bpe`): native byte-level BPE, always available.
//! * [`SequenceBpeTokenizer`] (`sequence-bpe`): BPE over named symbols.
//! * `RustBpeTokenizer` (`rustbpe`, behind the `rustbpe` feature): the
//!   reference backend, wrapping the rustbpe crate behind the same interface.
//!
//! All three implement `kute_core::Tokenizer`.

pub mod byte;
pub mod merge;
pub mod sequence;

#[cfg(feature = "rustbpe")]
pub mod rustbpe_backend;

pub use byte::BpeTokenizer;
pub use sequence::SequenceBpeTokenizer;

#[cfg(feature = "rustbpe")]
pub use rustbpe_backend::RustBpeTokenizer;

/// Register this crate's algorithms.
pub fn register(registry: &mut kute_core::Registry) {
    registry.register_algorithm(byte::ALGORITHM, BpeTokenizer::boxed);
    registry.register_algorithm(sequence::ALGORITHM, SequenceBpeTokenizer::boxed);
    #[cfg(feature = "rustbpe")]
    registry.register_algorithm(rustbpe_backend::ALGORITHM, RustBpeTokenizer::boxed);
}
