//! Byte Pair Encoding for KUTE.
//!
//! * [`BpeTokenizer`] (`bpe`): native byte-level BPE, always available.
//! * [`SequenceBpeTokenizer`] (`sequence-bpe`): BPE over named symbols.
//!
//! Both implement `kute_core::Tokenizer`. The rustbpe reference backend lives
//! in `backends/rustbpe` of the repository and is not published.

pub mod byte;
pub mod merge;
pub mod sequence;

pub use byte::BpeTokenizer;
pub use sequence::SequenceBpeTokenizer;

/// Register this crate's algorithms.
pub fn register(registry: &mut kute_core::Registry) {
    registry.register_algorithm(byte::ALGORITHM, BpeTokenizer::boxed);
    registry.register_algorithm(sequence::ALGORITHM, SequenceBpeTokenizer::boxed);
}
