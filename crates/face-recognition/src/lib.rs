//! Face embeddings (§4, §14).
//!
//! An embedding turns a face into a vector of numbers whose direction encodes
//! identity. Two pictures of the same player point in nearly the same
//! direction; two different players do not. Everything downstream — matching
//! against the player library, clustering strangers — is arithmetic on these
//! vectors.

pub mod align;
pub mod arcface;

use image::RgbImage;

pub use align::{align_face, align_from_bbox, ALIGNED_SIZE, ARCFACE_TEMPLATE};
pub use arcface::ArcFaceEmbedder;
pub use skwad_face_detection::{Accelerator, Detection, SessionConfig};

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("model file not found: {0}")]
    ModelMissing(String),
    #[error("ONNX Runtime error: {0}")]
    Runtime(String),
    #[error("unexpected model output: {0}")]
    BadOutput(String),
    #[error("face could not be aligned")]
    AlignmentFailed,
}

pub type Result<T> = std::result::Result<T, EmbedError>;

impl From<skwad_face_detection::FaceError> for EmbedError {
    fn from(e: skwad_face_detection::FaceError) -> Self {
        use skwad_face_detection::FaceError;
        match e {
            FaceError::ModelMissing(m) => EmbedError::ModelMissing(m),
            FaceError::BadOutput(m) => EmbedError::BadOutput(m),
            other => EmbedError::Runtime(other.to_string()),
        }
    }
}

/// An L2-normalised identity vector. Because it is unit length, the cosine
/// similarity between two embeddings is just their dot product.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding(pub Vec<f32>);

impl Embedding {
    pub fn dim(&self) -> usize {
        self.0.len()
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    pub fn into_vec(self) -> Vec<f32> {
        self.0
    }

    /// Scales the vector to unit length. A zero vector is left alone rather
    /// than producing NaNs.
    pub fn normalise(mut values: Vec<f32>) -> Self {
        let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 1e-12 {
            for v in values.iter_mut() {
                *v /= norm;
            }
        }
        Embedding(values)
    }

    /// Cosine similarity in −1..1. Returns 0 for mismatched dimensions rather
    /// than panicking, so a model change cannot crash a running import.
    pub fn similarity(&self, other: &Embedding) -> f32 {
        if self.0.len() != other.0.len() {
            return 0.0;
        }
        self.0.iter().zip(&other.0).map(|(a, b)| a * b).sum::<f32>().clamp(-1.0, 1.0)
    }
}

/// Anything that can turn an aligned face into an embedding.
pub trait FaceEmbedder: Send {
    /// Embeds one face, given the full frame and the detection within it.
    fn embed(&mut self, image: &RgbImage, detection: &Detection) -> Result<Embedding>;

    /// Embeds several faces from the same frame. Implementations that support
    /// batching should override this; the default runs them one at a time.
    fn embed_batch(&mut self, image: &RgbImage, detections: &[Detection]) -> Vec<Result<Embedding>> {
        detections.iter().map(|d| self.embed(image, d)).collect()
    }

    fn name(&self) -> &str;

    fn dim(&self) -> usize;
}

/// Crops and aligns a detected face into the square the embedder expects.
pub fn prepare_face(image: &RgbImage, detection: &Detection) -> RgbImage {
    match detection.landmarks.as_ref().and_then(|lm| align_face(image, lm)) {
        Some(aligned) => aligned,
        None => align_from_bbox(image, &detection.bbox),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalisation_produces_unit_length() {
        let e = Embedding::normalise(vec![3.0, 4.0]);
        assert!((e.0[0] - 0.6).abs() < 1e-6);
        assert!((e.0[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn a_zero_vector_survives_normalisation() {
        let e = Embedding::normalise(vec![0.0, 0.0, 0.0]);
        assert!(e.0.iter().all(|v| v.is_finite()));
        assert_eq!(e.similarity(&e), 0.0);
    }

    #[test]
    fn identical_embeddings_are_maximally_similar() {
        let e = Embedding::normalise(vec![0.012, -0.311, 0.882, 0.4]);
        assert!((e.similarity(&e) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn orthogonal_embeddings_score_zero() {
        let a = Embedding::normalise(vec![1.0, 0.0]);
        let b = Embedding::normalise(vec![0.0, 1.0]);
        assert!(a.similarity(&b).abs() < 1e-6);
    }

    #[test]
    fn opposite_embeddings_score_minus_one() {
        let a = Embedding::normalise(vec![1.0, 0.0]);
        let b = Embedding::normalise(vec![-1.0, 0.0]);
        assert!((a.similarity(&b) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn mismatched_dimensions_are_not_a_panic() {
        let a = Embedding::normalise(vec![1.0, 0.0]);
        let b = Embedding::normalise(vec![1.0, 0.0, 0.0]);
        assert_eq!(a.similarity(&b), 0.0);
    }

    #[test]
    fn prepare_face_falls_back_when_landmarks_are_missing() {
        use skwad_face_detection::Rect;
        let image = RgbImage::new(200, 200);
        let detection = Detection {
            bbox: Rect { x1: 50.0, y1: 50.0, x2: 150.0, y2: 150.0 },
            score: 0.9,
            landmarks: None,
        };
        assert_eq!(prepare_face(&image, &detection).dimensions(), (ALIGNED_SIZE, ALIGNED_SIZE));
    }
}
