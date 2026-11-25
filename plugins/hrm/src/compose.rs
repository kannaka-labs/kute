//! Composing token representations into one vector for a sequence
//! (specification F1). Experimental, and research: these are the standard
//! operations of vector symbolic architectures over the phase code in
//! this crate, here so they can be measured, not because they have been
//! shown to be useful.
//!
//! Three ways to turn a sequence of tokens into one vector:
//!
//! * [`Algorithm::Bundle`]: the sum. Order is lost: `A B` and `B A` are
//!   the same vector. The baseline that order-sensitive methods must beat.
//! * [`Algorithm::PermuteBundle`]: the token at position `p` is rotated
//!   by `p` places, then summed. Order is kept, and the token at a known
//!   position can be looked for again.
//! * [`Algorithm::BindChain`]: the rotated tokens are multiplied
//!   elementwise (their phases add). Order is kept; the result resembles
//!   none of its constituents, and one can be recovered only when all the
//!   others are known.
//!
//! Every result is scaled to unit energy, so the real part of the inner
//! product of two results is a similarity in [-1, 1].
//!
//! What this is not: learned, or semantic. Two tokens are similar only if
//! they are the same token. Nothing here says a medium that consumes
//! these vectors computes anything; see `docs/representations.md` for
//! what was measured.

use crate::{inner_product, HrmPhaseMapping, MAPPING_ID, MAPPING_VERSION};
use kute_types::{KuteError, Result};

pub const COMPOSE_VERSION: &str = "0.1.0";

/// Two implementations of one identity agree if the similarity of any two
/// unit-energy vectors they produce differs by no more than this. Sums
/// are in `f32`, in index order; sines and cosines come from the
/// platform's library, which is where implementations differ.
pub const PORTABLE_TOLERANCE: f32 = 1e-4;

/// Longest sequence composed into one vector.
pub const MAX_ITEMS: usize = 4096;

/// A token as unit phasors, before any scaling.
pub type Item = Vec<(f32, f32)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algorithm {
    Bundle,
    PermuteBundle,
    BindChain,
}

impl Algorithm {
    pub const ALL: [Algorithm; 3] = [Algorithm::Bundle, Algorithm::PermuteBundle, Algorithm::BindChain];

    pub fn name(self) -> &'static str {
        match self {
            Algorithm::Bundle => "bundle",
            Algorithm::PermuteBundle => "permute-bundle",
            Algorithm::BindChain => "bind-chain",
        }
    }
}

pub struct Composer {
    algorithm: Algorithm,
    mapping: HrmPhaseMapping,
    dimension: usize,
}

impl Composer {
    pub fn new(algorithm: Algorithm, dimension: usize) -> Result<Self> {
        Ok(Self { algorithm, mapping: HrmPhaseMapping::new(dimension)?, dimension })
    }

    /// Everything that fixes the vectors: algorithm, its version, the
    /// dimension, its configuration, and the token mapping underneath with
    /// its version. Two vectors are comparable only under one identity.
    pub fn identity(&self) -> String {
        format!("kannaka:hrm:compose:{}@{COMPOSE_VERSION}/dimension={}/rotation=1-per-position/base={MAPPING_ID}@{MAPPING_VERSION}", self.algorithm.name(), self.dimension)
    }

    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// The token with this content hash, as unit phasors.
    pub fn item(&self, content_hash: &str) -> Item {
        self.mapping.phases(content_hash).into_iter().map(|p| (p.cos(), p.sin())).collect()
    }

    fn check(&self, vector: &[(f32, f32)]) -> Result<()> {
        if vector.len() != self.dimension {
            return Err(KuteError::InvalidInput(format!("a vector of dimension {} under a composition of dimension {}", vector.len(), self.dimension)));
        }
        Ok(())
    }

    /// The sequence as one vector of unit energy.
    pub fn compose(&self, items: &[&[(f32, f32)]]) -> Result<Vec<(f32, f32)>> {
        if items.is_empty() || items.len() > MAX_ITEMS {
            return Err(KuteError::InvalidInput(format!("a composition takes 1 to {MAX_ITEMS} items")));
        }
        let d = self.dimension;
        let mut out = match self.algorithm {
            Algorithm::BindChain => vec![(1.0f32, 0.0f32); d],
            _ => vec![(0.0f32, 0.0f32); d],
        };
        for (position, item) in items.iter().enumerate() {
            self.check(item)?;
            let shift = match self.algorithm {
                Algorithm::Bundle => 0,
                _ => position % d,
            };
            for (i, &(re, im)) in item.iter().enumerate() {
                let slot = &mut out[(i + shift) % d];
                *slot = match self.algorithm {
                    Algorithm::BindChain => (slot.0 * re - slot.1 * im, slot.0 * im + slot.1 * re),
                    _ => (slot.0 + re, slot.1 + im),
                };
            }
        }
        normalize(&mut out)?;
        Ok(out)
    }

    /// Which of `candidates` is the constituent at `position`. For the two
    /// sums, the nearest candidate to the composition (rotated back, when
    /// position matters). For the chain, `known` must hold every other
    /// constituent with its position; they are divided out first.
    pub fn recover(&self, composed: &[(f32, f32)], position: usize, known: &[(usize, &[(f32, f32)])], candidates: &[Item]) -> Result<usize> {
        self.check(composed)?;
        if candidates.is_empty() {
            return Err(KuteError::InvalidInput("no candidates to recover from".into()));
        }
        let d = self.dimension;
        let mut residue = composed.to_vec();
        if self.algorithm == Algorithm::BindChain {
            for &(at, item) in known {
                self.check(item)?;
                for (i, &(re, im)) in item.iter().enumerate() {
                    // Multiply by the conjugate: the phase is taken away.
                    let slot = &mut residue[(i + at % d) % d];
                    *slot = (slot.0 * re + slot.1 * im, slot.1 * re - slot.0 * im);
                }
            }
        }
        let shift = if self.algorithm == Algorithm::Bundle { 0 } else { position % d };
        let mut best = (0usize, f32::NEG_INFINITY);
        for (index, candidate) in candidates.iter().enumerate() {
            self.check(candidate)?;
            let score: f32 = candidate.iter().enumerate().map(|(i, &(re, im))| residue[(i + shift) % d].0 * re + residue[(i + shift) % d].1 * im).sum();
            if score > best.1 {
                best = (index, score);
            }
        }
        Ok(best.0)
    }
}

/// Scale to unit energy. A vector of no energy has no direction.
pub fn normalize(vector: &mut [(f32, f32)]) -> Result<()> {
    let energy: f64 = vector.iter().map(|&(re, im)| f64::from(re) * f64::from(re) + f64::from(im) * f64::from(im)).sum();
    if !(energy.is_finite() && energy > 0.0) {
        return Err(KuteError::InvalidInput("a vector with no energy cannot be scaled".into()));
    }
    let scale = (1.0 / energy.sqrt()) as f32;
    for v in vector.iter_mut() {
        *v = (v.0 * scale, v.1 * scale);
    }
    Ok(())
}

/// The real part of the inner product: 1 for the same unit vector, near 0
/// for unrelated ones.
pub fn similarity(a: &[(f32, f32)], b: &[(f32, f32)]) -> Result<f32> {
    Ok(inner_product(a, b)?.0)
}
