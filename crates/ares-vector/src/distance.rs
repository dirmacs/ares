//! Distance metrics for vector similarity.
//!
//! Provides various distance/similarity metrics used for comparing vectors.

use std::fmt;

/// Distance metric for vector similarity calculations.
///
/// The choice of distance metric significantly affects search results:
///
/// - **Cosine**: Best for normalized embeddings (most LLM embeddings).
/// - **Euclidean**: Best for raw feature vectors where magnitude matters.
/// - **DotProduct**: Best for vectors that are already normalized.
/// - **Manhattan**: Robust to outliers, good for sparse vectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DistanceMetric {
    /// Cosine similarity (1 - cosine_distance).
    ///
    /// Measures the angle between vectors, ignoring magnitude.
    /// Range: [-1, 1], where 1 means identical direction.
    ///
    /// Best for: Text embeddings, semantic similarity.
    #[default]
    Cosine,

    /// Euclidean (L2) distance.
    ///
    /// Measures the straight-line distance between vectors.
    /// Range: [0, ∞), where 0 means identical vectors.
    ///
    /// Best for: Image features, geographic coordinates.
    Euclidean,

    /// Dot product (inner product).
    ///
    /// Measures alignment of vectors including magnitude.
    /// Range: (-∞, ∞), where higher is more similar.
    ///
    /// Best for: Pre-normalized vectors, recommendation systems.
    DotProduct,

    /// Manhattan (L1) distance.
    ///
    /// Sum of absolute differences across dimensions.
    /// Range: [0, ∞), where 0 means identical vectors.
    ///
    /// Best for: Sparse vectors, grid-based navigation.
    Manhattan,
}

impl DistanceMetric {
    /// Compute the similarity score between two vectors.
    ///
    /// Returns a score where **higher is more similar** for all metrics.
    /// For distance-based metrics (Euclidean, Manhattan), this returns
    /// a transformed score in [0, 1] range.
    ///
    /// # Panics
    ///
    /// Panics if vectors have different lengths.
    #[inline]
    pub fn similarity(&self, a: &[f32], b: &[f32]) -> f32 {
        debug_assert_eq!(a.len(), b.len(), "Vector dimensions must match");

        match self {
            DistanceMetric::Cosine => cosine_similarity(a, b),
            DistanceMetric::Euclidean => {
                let dist = euclidean_distance(a, b);
                // Transform to similarity: 1 / (1 + dist)
                1.0 / (1.0 + dist)
            }
            DistanceMetric::DotProduct => dot_product(a, b),
            DistanceMetric::Manhattan => {
                let dist = manhattan_distance(a, b);
                // Transform to similarity: 1 / (1 + dist)
                1.0 / (1.0 + dist)
            }
        }
    }

    /// Compute the raw distance between two vectors.
    ///
    /// Returns a distance where **lower means more similar**.
    /// For similarity-based metrics (Cosine, DotProduct), this returns
    /// a transformed distance.
    #[inline]
    pub fn distance(&self, a: &[f32], b: &[f32]) -> f32 {
        debug_assert_eq!(a.len(), b.len(), "Vector dimensions must match");

        match self {
            DistanceMetric::Cosine => 1.0 - cosine_similarity(a, b),
            DistanceMetric::Euclidean => euclidean_distance(a, b),
            DistanceMetric::DotProduct => -dot_product(a, b), // Negate for distance
            DistanceMetric::Manhattan => manhattan_distance(a, b),
        }
    }

    /// Returns true if this metric is similarity-based (higher = more similar).
    pub fn is_similarity_based(&self) -> bool {
        matches!(self, DistanceMetric::Cosine | DistanceMetric::DotProduct)
    }

    /// Returns true if this metric is distance-based (lower = more similar).
    pub fn is_distance_based(&self) -> bool {
        matches!(self, DistanceMetric::Euclidean | DistanceMetric::Manhattan)
    }

    /// Get the name of this distance metric.
    pub fn name(&self) -> &'static str {
        match self {
            DistanceMetric::Cosine => "cosine",
            DistanceMetric::Euclidean => "euclidean",
            DistanceMetric::DotProduct => "dot_product",
            DistanceMetric::Manhattan => "manhattan",
        }
    }
}

impl fmt::Display for DistanceMetric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

impl std::str::FromStr for DistanceMetric {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "cosine" | "cos" => Ok(DistanceMetric::Cosine),
            "euclidean" | "l2" | "euclid" => Ok(DistanceMetric::Euclidean),
            "dot" | "dot_product" | "dotproduct" | "inner" => Ok(DistanceMetric::DotProduct),
            "manhattan" | "l1" | "taxicab" => Ok(DistanceMetric::Manhattan),
            _ => Err(format!("Unknown distance metric: {}", s)),
        }
    }
}

// ============================================================================
// Optimized Distance Functions
// ============================================================================

/// Compute cosine similarity between two vectors.
///
/// Returns a value in [-1, 1] where 1 means identical direction.
#[inline]
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    // Manual loop unrolling for better performance
    let chunks = a.len() / 4;
    let remainder = a.len() % 4;

    for i in 0..chunks {
        let base = i * 4;
        dot += a[base] * b[base]
            + a[base + 1] * b[base + 1]
            + a[base + 2] * b[base + 2]
            + a[base + 3] * b[base + 3];
        norm_a += a[base] * a[base]
            + a[base + 1] * a[base + 1]
            + a[base + 2] * a[base + 2]
            + a[base + 3] * a[base + 3];
        norm_b += b[base] * b[base]
            + b[base + 1] * b[base + 1]
            + b[base + 2] * b[base + 2]
            + b[base + 3] * b[base + 3];
    }

    let start = chunks * 4;
    for i in 0..remainder {
        let idx = start + i;
        dot += a[idx] * b[idx];
        norm_a += a[idx] * a[idx];
        norm_b += b[idx] * b[idx];
    }

    let denom = (norm_a * norm_b).sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

/// Compute Euclidean (L2) distance between two vectors.
#[inline]
fn euclidean_distance(a: &[f32], b: &[f32]) -> f32 {
    let mut sum = 0.0f32;

    let chunks = a.len() / 4;
    let remainder = a.len() % 4;

    for i in 0..chunks {
        let base = i * 4;
        let d0 = a[base] - b[base];
        let d1 = a[base + 1] - b[base + 1];
        let d2 = a[base + 2] - b[base + 2];
        let d3 = a[base + 3] - b[base + 3];
        sum += d0 * d0 + d1 * d1 + d2 * d2 + d3 * d3;
    }

    let start = chunks * 4;
    for i in 0..remainder {
        let idx = start + i;
        let d = a[idx] - b[idx];
        sum += d * d;
    }

    sum.sqrt()
}

/// Compute dot product between two vectors.
#[inline]
fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    let mut sum = 0.0f32;

    let chunks = a.len() / 4;
    let remainder = a.len() % 4;

    for i in 0..chunks {
        let base = i * 4;
        sum += a[base] * b[base]
            + a[base + 1] * b[base + 1]
            + a[base + 2] * b[base + 2]
            + a[base + 3] * b[base + 3];
    }

    let start = chunks * 4;
    for i in 0..remainder {
        let idx = start + i;
        sum += a[idx] * b[idx];
    }

    sum
}

/// Compute Manhattan (L1) distance between two vectors.
#[inline]
fn manhattan_distance(a: &[f32], b: &[f32]) -> f32 {
    let mut sum = 0.0f32;

    for (x, y) in a.iter().zip(b.iter()) {
        sum += (x - y).abs();
    }

    sum
}

// ============================================================================
// HNSW Distance Adapter
// ============================================================================

use anndists::dist::distances::{DistCosine, DistDot, DistL1, DistL2};
use anndists::dist::Distance;

/// Trait for creating HNSW distance instances.
pub trait HnswDistance: Clone + Send + Sync + 'static {
    /// Create the HNSW distance function type.
    type Dist: Distance<f32> + Clone + Send + Sync + Default;

    /// Create a new instance of the distance function.
    fn create() -> Self::Dist;
}

/// Cosine distance adapter for HNSW.
#[derive(Clone)]
pub struct CosineDistance;

impl HnswDistance for CosineDistance {
    type Dist = DistCosine;
    fn create() -> Self::Dist {
        DistCosine {}
    }
}

/// Euclidean distance adapter for HNSW.
#[derive(Clone)]
pub struct EuclideanDistance;

impl HnswDistance for EuclideanDistance {
    type Dist = DistL2;
    fn create() -> Self::Dist {
        DistL2 {}
    }
}

/// Dot product distance adapter for HNSW.
#[derive(Clone)]
pub struct DotProductDistance;

impl HnswDistance for DotProductDistance {
    type Dist = DistDot;
    fn create() -> Self::Dist {
        DistDot {}
    }
}

/// Manhattan distance adapter for HNSW.
#[derive(Clone)]
pub struct ManhattanDistance;

impl HnswDistance for ManhattanDistance {
    type Dist = DistL1;
    fn create() -> Self::Dist {
        DistL1 {}
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_identical() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let sim = DistanceMetric::Cosine.similarity(&a, &b);
        assert!((sim - 1.0).abs() < 0.0001);
    }

    #[test]
    fn test_cosine_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = DistanceMetric::Cosine.similarity(&a, &b);
        assert!(sim.abs() < 0.0001);
    }

    #[test]
    fn test_cosine_opposite() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        let sim = DistanceMetric::Cosine.similarity(&a, &b);
        assert!((sim + 1.0).abs() < 0.0001);
    }

    #[test]
    fn test_euclidean_identical() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];
        let dist = DistanceMetric::Euclidean.distance(&a, &b);
        assert!(dist.abs() < 0.0001);
    }

    #[test]
    fn test_euclidean_distance() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let dist = DistanceMetric::Euclidean.distance(&a, &b);
        assert!((dist - 1.0).abs() < 0.0001);
    }

    #[test]
    fn test_manhattan_distance() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let dist = DistanceMetric::Manhattan.distance(&a, &b);
        assert!((dist - 6.0).abs() < 0.0001);
    }

    #[test]
    fn test_dot_product() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        let sim = DistanceMetric::DotProduct.similarity(&a, &b);
        // 1*4 + 2*5 + 3*6 = 4 + 10 + 18 = 32
        assert!((sim - 32.0).abs() < 0.0001);
    }

    #[test]
    fn test_metric_from_str() {
        assert_eq!("cosine".parse::<DistanceMetric>().unwrap(), DistanceMetric::Cosine);
        assert_eq!("l2".parse::<DistanceMetric>().unwrap(), DistanceMetric::Euclidean);
        assert_eq!("dot".parse::<DistanceMetric>().unwrap(), DistanceMetric::DotProduct);
        assert_eq!("manhattan".parse::<DistanceMetric>().unwrap(), DistanceMetric::Manhattan);
    }
}
