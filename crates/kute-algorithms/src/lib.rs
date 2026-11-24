//! Baseline algorithms (FR-002, priority P0 besides BPE):
//!
//! * `bytes`: one token per byte. The lossless fallback and the baseline
//!   every other vocabulary is measured against.
//! * `char`: one token per Unicode character seen in training, with the 256
//!   byte tokens underneath so unseen characters and invalid UTF-8 still
//!   round-trip.
//! * `dictionary`: a fixed list of entries. Over bytes it matches the longest
//!   entry greedily and falls back to byte tokens; over symbols it is a plain
//!   lookup that refuses unknown symbols.

mod table;

pub use table::{ByteTokenizer, CharTokenizer, DictionaryTokenizer};

pub const BYTES: &str = "bytes";
pub const CHAR: &str = "char";
pub const DICTIONARY: &str = "dictionary";

/// Register this crate's algorithms.
pub fn register(registry: &mut kute_core::Registry) {
    registry.register_algorithm(BYTES, || Box::new(ByteTokenizer::default()));
    registry.register_algorithm(CHAR, || Box::new(CharTokenizer::default()));
    registry.register_algorithm(DICTIONARY, || Box::new(DictionaryTokenizer::default()));
}
