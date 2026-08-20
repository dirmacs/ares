//! Distance metrics for vector similarity.
//!
//! Provides various distance/similarity metrics used for comparing vectors.

use crate::error::{Error, Result};
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
#[derive(serde::Serialize, serde::Deserialize)]
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
    /// Mathematical formula for this metric's distance computation.
    pub fn formula(&self) -> &'static str {
        match self {
            DistanceMetric::Cosine => "1 - (a·b) / (||a|| · ||b||)",
            DistanceMetric::Euclidean => "√(Σ(aᵢ - bᵢ)²)",
            DistanceMetric::DotProduct => "-(a·b)",
            DistanceMetric::Manhattan => "Σ|aᵢ - bᵢ|",
        }
    }

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
                let dist = l2_distance(a, b);
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
            DistanceMetric::Euclidean => l2_distance(a, b),
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

/// Display wrapper that renders the mathematical formula for a metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DistanceFormula(pub DistanceMetric);

impl fmt::Display for DistanceFormula {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.formula())
    }
}

impl std::str::FromStr for DistanceMetric {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "cosine" | "cos" => Ok(DistanceMetric::Cosine),
            "euclidean" | "l2" | "euclid" => Ok(DistanceMetric::Euclidean),
            "dot" | "dot_product" | "dotproduct" | "inner" => Ok(DistanceMetric::DotProduct),
            "manhattan" | "l1" | "taxicab" => Ok(DistanceMetric::Manhattan),
            _ => Err(format!("Unknown distance metric: {s}")),
        }
    }
}

// ============================================================================
// Pure vector helpers
// ============================================================================

/// Validate that `vector` has `expected` dimensions and finite values.
pub fn validate_dimensions(vector: &[f32], expected: usize) -> Result<()> {
    if vector.len() != expected {
        return Err(Error::DimensionMismatch {
            expected,
            actual: vector.len(),
        });
    }
    if expected == 0 {
        return Err(Error::InvalidVector("Dimensions must be > 0".into()));
    }
    if vector.iter().any(|v| !v.is_finite()) {
        return Err(Error::InvalidVector(
            "Vector contains NaN or Inf".to_string(),
        ));
    }
    Ok(())
}

/// Validate that two vectors share the same dimension and contain finite values.
pub fn validate_dimension_pair(a: &[f32], b: &[f32]) -> Result<()> {
    if a.len() != b.len() {
        return Err(Error::DimensionMismatch {
            expected: a.len(),
            actual: b.len(),
        });
    }
    validate_dimensions(a, a.len())
}

/// L2-normalize `vector` in place.
pub fn normalize_vector(vector: &mut [f32]) -> Result<()> {
    validate_dimensions(vector, vector.len())?;

    let norm_sq: f32 = vector.iter().map(|x| x * x).sum();
    if !norm_sq.is_finite() {
        return Err(Error::NumericalInstability(
            "Norm squared overflow".to_string(),
        ));
    }

    let norm = norm_sq.sqrt();
    if norm == 0.0 {
        return Err(Error::InvalidVector("Cannot normalize zero vector".into()));
    }
    if !norm.is_finite() {
        return Err(Error::NumericalInstability("Norm overflow".to_string()));
    }

    for x in vector.iter_mut() {
        *x /= norm;
    }
    Ok(())
}

/// Compute cosine similarity between two vectors.
///
/// Returns a value in [-1, 1] where 1 means identical direction.
/// Zero-norm vectors yield 0.0.
#[inline]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

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
pub fn l2_distance(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());

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

    if !sum.is_finite() {
        return f32::INFINITY;
    }

    sum.sqrt()
}

/// Compute dot product between two vectors.
#[inline]
pub fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());

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
// Scalar reference implementations (for equivalence tests)
// ============================================================================

#[cfg(test)]
#[inline]
fn cosine_similarity_scalar(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    let denom = norm_a * norm_b;
    if denom == 0.0 { 0.0 } else { dot / denom }
}

#[cfg(test)]
#[inline]
fn l2_distance_scalar(a: &[f32], b: &[f32]) -> f32 {
    let sum: f32 = a
        .iter()
        .zip(b)
        .map(|(x, y)| {
            let d = x - y;
            d * d
        })
        .sum();
    if !sum.is_finite() {
        return f32::INFINITY;
    }
    sum.sqrt()
}

#[cfg(test)]
#[inline]
fn dot_product_scalar(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
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

/// pgvector distance operator for supported metrics.
pub fn distance_operator(metric: DistanceMetric) -> Option<&'static str> {
    match metric {
        DistanceMetric::Cosine => Some("<=>"),
        DistanceMetric::Euclidean => Some("<->"),
        DistanceMetric::DotProduct => Some("<#>"),
        DistanceMetric::Manhattan => None,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    const EPS: f32 = 1e-4;

    fn approx_eq(a: f32, b: f32) -> bool {
        (a - b).abs() < EPS
    }

    // ------------------------------------------------------------------------
    // DistanceMetric: serde, Display, Clone, Debug
    // ------------------------------------------------------------------------

    #[cfg(feature = "serde")]
    #[test]
    fn metric_serde_roundtrip_cosine() {
        let metric = DistanceMetric::Cosine;
        let json = serde_json::to_string(&metric).unwrap();
        let decoded: DistanceMetric = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, metric);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn metric_serde_roundtrip_euclidean() {
        let metric = DistanceMetric::Euclidean;
        let json = serde_json::to_string(&metric).unwrap();
        let decoded: DistanceMetric = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, DistanceMetric::Euclidean);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn metric_serde_roundtrip_dot_product() {
        let metric = DistanceMetric::DotProduct;
        let json = serde_json::to_string(&metric).unwrap();
        let decoded: DistanceMetric = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, DistanceMetric::DotProduct);
    }

    #[test]
    fn metric_debug_clone() {
        let metric = DistanceMetric::Cosine;
        let cloned = metric;
        assert_eq!(format!("{metric:?}"), format!("{cloned:?}"));
        assert_eq!(cloned, metric);
    }

    #[test]
    fn metric_display_name() {
        assert_eq!(DistanceMetric::Cosine.to_string(), "cosine");
        assert_eq!(DistanceMetric::Euclidean.to_string(), "euclidean");
        assert_eq!(format!("{}", DistanceMetric::DotProduct), "dot_product");
    }

    #[test]
    fn metric_formula_display() {
        assert_eq!(
            DistanceFormula(DistanceMetric::Cosine).to_string(),
            "1 - (a·b) / (||a|| · ||b||)"
        );
        assert_eq!(
            DistanceFormula(DistanceMetric::Euclidean).to_string(),
            "√(Σ(aᵢ - bᵢ)²)"
        );
        assert_eq!(
            DistanceFormula(DistanceMetric::DotProduct).to_string(),
            "-(a·b)"
        );
        assert_eq!(
            DistanceFormula(DistanceMetric::Manhattan).to_string(),
            "Σ|aᵢ - bᵢ|"
        );
    }

    #[test]
    fn test_metric_from_str() {
        assert_eq!(
            "cosine".parse::<DistanceMetric>().unwrap(),
            DistanceMetric::Cosine
        );
        assert_eq!(
            "l2".parse::<DistanceMetric>().unwrap(),
            DistanceMetric::Euclidean
        );
        assert_eq!(
            "dot".parse::<DistanceMetric>().unwrap(),
            DistanceMetric::DotProduct
        );
        assert!("unknown".parse::<DistanceMetric>().is_err());
    }

    #[test]
    fn test_default_metric_is_cosine() {
        assert_eq!(DistanceMetric::default(), DistanceMetric::Cosine);
    }

    #[test]
    fn test_metric_kind_flags() {
        assert!(DistanceMetric::Cosine.is_similarity_based());
        assert!(DistanceMetric::DotProduct.is_similarity_based());
        assert!(!DistanceMetric::Euclidean.is_similarity_based());
        assert!(DistanceMetric::Manhattan.is_distance_based());
    }

    // ------------------------------------------------------------------------
    // cosine_similarity
    // ------------------------------------------------------------------------

    #[test]
    fn cosine_unit_vectors_identical() {
        let a = [1.0f32, 0.0, 0.0];
        let b = [1.0, 0.0, 0.0];
        assert!(approx_eq(cosine_similarity(&a, &b), 1.0));
    }

    #[test]
    fn cosine_orthogonal_vectors() {
        let a = [1.0f32, 0.0, 0.0];
        let b = [0.0, 1.0, 0.0];
        assert!(cosine_similarity(&a, &b).abs() < EPS);
    }

    #[test]
    fn cosine_opposite_vectors() {
        let a = [1.0f32, 0.0, 0.0];
        let b = [-1.0, 0.0, 0.0];
        assert!(approx_eq(cosine_similarity(&a, &b), -1.0));
    }

    #[test]
    fn cosine_zero_vectors() {
        let a = [0.0f32, 0.0, 0.0];
        let b = [0.0, 0.0, 0.0];
        assert!(cosine_similarity(&a, &b).abs() < EPS);
    }

    #[test]
    fn cosine_one_zero_vector() {
        let a = [0.0f32, 0.0];
        let b = [1.0, 1.0];
        assert!(cosine_similarity(&a, &b).abs() < EPS);
    }

    #[test]
    fn cosine_parallel_large_scale() {
        let a = [1_000.0f32, 2_000.0, 3_000.0];
        let b = [2_000.0, 4_000.0, 6_000.0];
        let sim = cosine_similarity(&a, &b);
        assert!(approx_eq(sim, 1.0));
        assert!((-1.0..=1.0).contains(&sim));
    }

    // ------------------------------------------------------------------------
    // l2_distance
    // ------------------------------------------------------------------------

    #[test]
    fn l2_identical_vectors_zero() {
        let a = [1.0f32, 2.0, 3.0];
        let b = [1.0, 2.0, 3.0];
        assert!(l2_distance(&a, &b).abs() < EPS);
    }

    #[test]
    fn l2_unit_distance() {
        let a = [0.0f32, 0.0, 0.0];
        let b = [1.0, 0.0, 0.0];
        assert!(approx_eq(l2_distance(&a, &b), 1.0));
    }

    #[test]
    fn l2_large_distance() {
        let a = [0.0f32, 0.0];
        let b = [3.0, 4.0];
        assert!(approx_eq(l2_distance(&a, &b), 5.0));
    }

    #[test]
    fn l2_overflow_returns_infinity() {
        let a = [f32::MAX, 0.0];
        let b = [0.0, 0.0];
        assert!(l2_distance(&a, &b).is_infinite());
    }

    // ------------------------------------------------------------------------
    // dot_product
    // ------------------------------------------------------------------------

    #[test]
    fn dot_product_positive() {
        let a = [1.0f32, 2.0, 3.0];
        let b = [4.0, 5.0, 6.0];
        assert!(approx_eq(dot_product(&a, &b), 32.0));
    }

    #[test]
    fn dot_product_negative() {
        let a = [1.0f32, 0.0];
        let b = [-1.0, 0.0];
        assert!(approx_eq(dot_product(&a, &b), -1.0));
    }

    #[test]
    fn dot_product_zero_vectors() {
        let a = [0.0f32, 0.0];
        let b = [0.0, 0.0];
        assert!(dot_product(&a, &b).abs() < EPS);
    }

    #[test]
    fn dot_product_magnitude_scaling() {
        let a = [2.0f32, 0.0];
        let b = [3.0f32, 0.0];
        assert!(approx_eq(dot_product(&a, &b), 6.0));
        let unit_a = [1.0f32, 0.0];
        let unit_b = [1.0f32, 0.0];
        assert!(approx_eq(dot_product(&unit_a, &unit_b), 1.0));
    }

    // ------------------------------------------------------------------------
    // validate_dimensions
    // ------------------------------------------------------------------------

    #[test]
    fn validate_dimensions_matching() {
        let v = [1.0f32, 2.0, 3.0];
        assert!(validate_dimensions(&v, 3).is_ok());
    }

    #[test]
    fn validate_dimensions_mismatch() {
        let v = [1.0f32, 2.0];
        let err = validate_dimensions(&v, 3).unwrap_err();
        assert!(matches!(
            err,
            Error::DimensionMismatch {
                expected: 3,
                actual: 2
            }
        ));
    }

    #[test]
    fn validate_dimensions_empty_vector() {
        let v: [f32; 0] = [];
        let err = validate_dimensions(&v, 0).unwrap_err();
        assert!(matches!(err, Error::InvalidVector(_)));
    }

    #[test]
    fn validate_dimensions_nan() {
        let v = [1.0f32, f32::NAN];
        let err = validate_dimensions(&v, 2).unwrap_err();
        assert!(matches!(err, Error::InvalidVector(_)));
    }

    #[test]
    fn validate_dimensions_inf() {
        let v = [1.0f32, f32::INFINITY];
        let err = validate_dimensions(&v, 2).unwrap_err();
        assert!(matches!(err, Error::InvalidVector(_)));
    }

    #[test]
    fn validate_dimension_pair_matching() {
        let a = [1.0f32, 2.0];
        let b = [3.0, 4.0];
        assert!(validate_dimension_pair(&a, &b).is_ok());
    }

    #[test]
    fn validate_dimension_pair_mismatch() {
        let a = [1.0f32];
        let b = [1.0, 2.0];
        let err = validate_dimension_pair(&a, &b).unwrap_err();
        assert!(matches!(
            err,
            Error::DimensionMismatch {
                expected: 1,
                actual: 2
            }
        ));
    }

    // ------------------------------------------------------------------------
    // normalize_vector
    // ------------------------------------------------------------------------

    #[test]
    fn normalize_vector_unit_norm() {
        let mut v = [3.0f32, 4.0];
        normalize_vector(&mut v).unwrap();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(approx_eq(norm, 1.0));
        assert!(approx_eq(v[0], 0.6));
        assert!(approx_eq(v[1], 0.8));
    }

    #[test]
    fn normalize_vector_zero_error() {
        let mut v = [0.0f32, 0.0];
        let err = normalize_vector(&mut v).unwrap_err();
        assert!(matches!(err, Error::InvalidVector(_)));
    }

    #[test]
    fn normalize_vector_negative_components() {
        let mut v = [-3.0f32, 4.0];
        normalize_vector(&mut v).unwrap();
        assert!(approx_eq(v[0], -0.6));
        assert!(approx_eq(v[1], 0.8));
    }

    // ------------------------------------------------------------------------
    // Scalar vs optimized equivalence
    // ------------------------------------------------------------------------

    #[test]
    fn scalar_cosine_matches_optimized() {
        let a: Vec<f32> = (0..65).map(|i| (i as f32 * 0.1).sin()).collect();
        let b: Vec<f32> = (0..65).map(|i| (i as f32 * 0.2).cos()).collect();
        let opt = cosine_similarity(&a, &b);
        let scalar = cosine_similarity_scalar(&a, &b);
        assert!(approx_eq(opt, scalar));
    }

    #[test]
    fn scalar_l2_matches_optimized() {
        let a: Vec<f32> = (0..65).map(|i| i as f32).collect();
        let b: Vec<f32> = (0..65).map(|i| (i as f32) + 0.5).collect();
        assert!(approx_eq(l2_distance(&a, &b), l2_distance_scalar(&a, &b)));
    }

    #[test]
    fn scalar_dot_matches_optimized() {
        let a: Vec<f32> = (0..65).map(|i| i as f32 * 0.01).collect();
        let b: Vec<f32> = (0..65).map(|i| (65 - i) as f32 * 0.01).collect();
        assert!(approx_eq(dot_product(&a, &b), dot_product_scalar(&a, &b)));
    }

    // ------------------------------------------------------------------------
    // Error variant display (distance-related paths)
    // ------------------------------------------------------------------------

    #[test]
    fn error_dimension_mismatch_display() {
        let err = Error::DimensionMismatch {
            expected: 384,
            actual: 128,
        };
        assert_eq!(
            err.to_string(),
            "Dimension mismatch: expected 384, got 128"
        );
    }

    #[test]
    fn error_invalid_vector_display() {
        let err = Error::InvalidVector("contains NaN".into());
        assert_eq!(err.to_string(), "Invalid vector: contains NaN");
    }

    #[test]
    fn error_numerical_instability_display() {
        let err = Error::NumericalInstability("norm overflow".into());
        assert_eq!(
            err.to_string(),
            "Numerical instability: norm overflow"
        );
    }

    // ------------------------------------------------------------------------
    // DistanceMetric integration (existing coverage)
    // ------------------------------------------------------------------------

    #[test]
    fn test_cosine_identical() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!(approx_eq(DistanceMetric::Cosine.similarity(&a, &b), 1.0));
    }

    #[test]
    fn test_cosine_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!(DistanceMetric::Cosine.similarity(&a, &b).abs() < EPS);
    }

    #[test]
    fn test_euclidean_identical() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];
        assert!(DistanceMetric::Euclidean.distance(&a, &b).abs() < EPS);
    }

    #[test]
    fn test_manhattan_distance() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        assert!(approx_eq(DistanceMetric::Manhattan.distance(&a, &b), 6.0));
    }

    #[test]
    fn test_dot_product_metric() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        assert!(approx_eq(DistanceMetric::DotProduct.similarity(&a, &b), 32.0));
    }

    #[test]
    fn test_single_dimension_vectors() {
        let a = vec![3.0];
        let b = vec![4.0];
        assert!(approx_eq(DistanceMetric::Euclidean.distance(&a, &b), 1.0));
        assert!(approx_eq(DistanceMetric::DotProduct.similarity(&a, &b), 12.0));
    }

    #[test]
    fn test_large_vectors_unrolled_paths() {
        let n = 128;
        let a: Vec<f32> = (0..n).map(|i| i as f32 * 0.01).collect();
        let b = a.clone();
        assert!(DistanceMetric::Euclidean.distance(&a, &b).abs() < EPS);
        assert!(approx_eq(DistanceMetric::Cosine.similarity(&a, &b), 1.0));
    }

    #[test]
    fn test_euclidean_and_manhattan_similarity_transform() {
        let a = vec![0.0, 0.0];
        let b = vec![3.0, 4.0];
        let euclid_sim = DistanceMetric::Euclidean.similarity(&a, &b);
        assert!(approx_eq(euclid_sim, 1.0 / 6.0));
        let manhattan_sim = DistanceMetric::Manhattan.similarity(&a, &b);
        assert!(approx_eq(manhattan_sim, 1.0 / 8.0));
    }

    #[test]
    fn test_hnsw_distance_adapters() {
        use anndists::dist::Distance;

        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![0.0f32, 1.0, 0.0];
        let same = vec![1.0f32, 0.0, 0.0];

        let cosine = CosineDistance::create();
        assert!(approx_eq(cosine.eval(&a, &b), 1.0));
        assert!(cosine.eval(&same, &a).abs() < EPS);

        let l2 = EuclideanDistance::create();
        assert!(approx_eq(l2.eval(&a, &b), std::f32::consts::SQRT_2));
        assert!(l2.eval(&same, &a).abs() < EPS);

        let dot = DotProductDistance::create();
        assert!(approx_eq(dot.eval(&a, &b), 1.0));
        assert!(dot.eval(&same, &a).abs() < EPS);

        let l1 = ManhattanDistance::create();
        assert!(approx_eq(l1.eval(&a, &b), 2.0));
        assert!(l1.eval(&same, &a).abs() < EPS);
    }
    #[test]
    fn distance_operator_maps_pgvector_tokens() {
        assert_eq!(distance_operator(DistanceMetric::Euclidean), Some("<->"));
        assert_eq!(distance_operator(DistanceMetric::DotProduct), Some("<#>"));
        assert_eq!(distance_operator(DistanceMetric::Cosine), Some("<=>"));
        assert_eq!(distance_operator(DistanceMetric::Manhattan), None);
    }

    #[test]
    fn distance_metric_serde_roundtrip() {
        for metric in [
            DistanceMetric::Cosine,
            DistanceMetric::Euclidean,
            DistanceMetric::DotProduct,
            DistanceMetric::Manhattan,
        ] {
            let json = serde_json::to_string(&metric).expect("serialize");
            let restored: DistanceMetric = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(restored, metric);
        }
    }

}
