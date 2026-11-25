//! Experimental HRM adapter (PRD 8.1, ADR-0012).
//!
//! The contract is `kute_core::RepresentationBackend`: a versioned, repeatable
//! mapping from a token to a complex-valued vector that an HRM-style medium
//! can take as input. This crate ships one reference mapping so the contract
//! can be exercised end to end.
//!
//! What this is: a deterministic, content-addressed phase code. The same token
//! payload always maps to the same vector, in any vocabulary, on any machine.
//!
//! What this is not: evidence of anything about HRM. The vectors are
//! pseudo-random phases derived from a hash. They carry no learned or semantic
//! structure, and KUTE makes no claim about the physical interpretation or
//! validity of the dynamics that consume them. Whether resonance mappings
//! improve retrieval is an open hypothesis (PRD section 12), to be tested
//! against a baseline with this as the repeatable input.

pub mod compose;

use kute_core::{Representation, RepresentationBackend};
use kute_types::{KuteError, RepresentationRef, Result, Vocabulary};
use sha2::{Digest, Sha256};

pub const MAPPING_ID: &str = "kannaka:hrm:phase-hash";
pub const MAPPING_VERSION: &str = "0.1.0";

/// Maps each token to `dimension` unit phasors scaled to unit total energy.
/// Phases come from SHA-256 of (mapping id, mapping version, token content
/// hash, block counter), so the mapping is fixed by its id and version.
pub struct HrmPhaseMapping {
    dimension: usize,
}

impl HrmPhaseMapping {
    pub const MAX_DIMENSION: usize = 65_536;

    pub fn new(dimension: usize) -> Result<Self> {
        if dimension == 0 || dimension > Self::MAX_DIMENSION {
            return Err(KuteError::InvalidConfig(format!("HRM mapping dimension must be 1 to {}", Self::MAX_DIMENSION)));
        }
        Ok(Self { dimension })
    }

    /// The reference to record in a vocabulary that is used with this mapping.
    pub fn reference(&self) -> RepresentationRef {
        RepresentationRef {
            mapping_id: MAPPING_ID.to_string(),
            mapping_version: MAPPING_VERSION.to_string(),
            kind: format!("complex_vector/{}", self.dimension),
            uri: None,
        }
    }

    /// Phases in radians for a token content hash.
    pub fn phases(&self, content_hash: &str) -> Vec<f32> {
        let mut phases = Vec::with_capacity(self.dimension);
        let mut block: u64 = 0;
        while phases.len() < self.dimension {
            let mut h = Sha256::new();
            h.update(MAPPING_ID.as_bytes());
            h.update([0]);
            h.update(MAPPING_VERSION.as_bytes());
            h.update([0]);
            h.update(content_hash.as_bytes());
            h.update(block.to_le_bytes());
            for chunk in h.finalize().chunks_exact(4) {
                if phases.len() == self.dimension {
                    break;
                }
                let x = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                // 24 bits of the hash, so the value is exact in f32.
                phases.push((x >> 8) as f32 / (1u32 << 24) as f32 * std::f32::consts::TAU);
            }
            block += 1;
        }
        phases
    }
}

impl RepresentationBackend for HrmPhaseMapping {
    fn mapping_id(&self) -> &str {
        MAPPING_ID
    }

    fn mapping_version(&self) -> &str {
        MAPPING_VERSION
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn map(&self, vocabulary: &Vocabulary, token_id: u32) -> Result<Representation> {
        let hash = vocabulary.token(token_id)?.payload.content_hash();
        let scale = 1.0 / (self.dimension as f32).sqrt();
        Ok(Representation::ComplexVector(
            self.phases(&hash).into_iter().map(|p| (p.cos() * scale, p.sin() * scale)).collect(),
        ))
    }
}

/// Hermitian inner product `<a, b>` of two complex vectors of the same
/// dimension. Vectors of different dimensions come from different mappings
/// and have no inner product; that is an error, not a truncation.
///
/// Sums are taken in index order in `f32`. Results agree across platforms to
/// within a few units in the last place per term; compare with a tolerance
/// of `1e-5 * dimension`, never for equality.
pub fn inner_product(a: &[(f32, f32)], b: &[(f32, f32)]) -> Result<(f32, f32)> {
    if a.len() != b.len() {
        return Err(KuteError::InvalidInput(format!("inner product of vectors with dimensions {} and {}", a.len(), b.len())));
    }
    Ok(a.iter().zip(b).fold((0.0, 0.0), |(re, im), (&(ar, ai), &(br, bi))| {
        (re + ar * br + ai * bi, im + ai * br - ar * bi)
    }))
}
