//! ArcFace-compatible embedding model over ONNX Runtime.
//!
//! Works with any 112×112 recognition model that takes an NCHW float tensor and
//! returns a single feature vector — the `w600k_r50` family, ArcFace ResNet
//! exports, and MobileFaceNet all fit. The output dimension is read from the
//! model rather than assumed, so a 128-d model drops in beside a 512-d one.

use std::path::Path;

use image::RgbImage;
use ort::session::Session;
use ort::value::Tensor;

use teo_face_detection::runtime::{build_session, SessionConfig};
use teo_face_detection::Detection;

use crate::align::ALIGNED_SIZE;
use crate::{prepare_face, EmbedError, Embedding, FaceEmbedder, Result};

/// ArcFace normalises to −1..1 rather than 0..1.
const PIXEL_MEAN: f32 = 127.5;
const PIXEL_SCALE: f32 = 127.5;

pub struct ArcFaceEmbedder {
    session: Session,
    input_name: String,
    dim: usize,
    name: String,
}

impl ArcFaceEmbedder {
    pub fn load(model_path: &Path, session_config: &SessionConfig) -> Result<Self> {
        let session = build_session(model_path, session_config)?;

        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| EmbedError::BadOutput("model declares no inputs".into()))?;

        // Prefer the declared output width; fall back to the ArcFace default
        // when the export leaves it dynamic. Either way `dim()` is corrected
        // after the first real inference.
        let dim = session
            .outputs()
            .first()
            .and_then(|o| match o.dtype() {
                ort::value::ValueType::Tensor { shape, .. } => shape.last().copied(),
                _ => None,
            })
            .filter(|d| *d > 0)
            .map(|d| d as usize)
            .unwrap_or(512);

        let name = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("arcface")
            .to_string();

        Ok(Self { session, input_name, dim, name })
    }

    /// Packs `count` aligned 112×112 crops into one NCHW batch tensor.
    fn to_tensor(crops: &[RgbImage]) -> Vec<f32> {
        let side = ALIGNED_SIZE as usize;
        let plane = side * side;
        let mut data = vec![0.0f32; crops.len() * 3 * plane];

        for (n, crop) in crops.iter().enumerate() {
            let base = n * 3 * plane;
            for y in 0..side.min(crop.height() as usize) {
                for x in 0..side.min(crop.width() as usize) {
                    let px = crop.get_pixel(x as u32, y as u32);
                    let offset = y * side + x;
                    for c in 0..3 {
                        data[base + c * plane + offset] = (px[c] as f32 - PIXEL_MEAN) / PIXEL_SCALE;
                    }
                }
            }
        }
        data
    }

    fn run(&mut self, crops: &[RgbImage]) -> Result<Vec<Embedding>> {
        if crops.is_empty() {
            return Ok(Vec::new());
        }

        let side = ALIGNED_SIZE as usize;
        let data = Self::to_tensor(crops);
        let tensor = Tensor::from_array(([crops.len(), 3, side, side], data))
            .map_err(|e| EmbedError::Runtime(e.to_string()))?;

        let outputs = self
            .session
            .run(ort::inputs![self.input_name.as_str() => tensor])
            .map_err(|e| EmbedError::Runtime(e.to_string()))?;

        let (shape, values) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| EmbedError::BadOutput(e.to_string()))?;

        let dim = shape.last().copied().unwrap_or(0).max(0) as usize;
        if dim == 0 || values.len() < dim * crops.len() {
            return Err(EmbedError::BadOutput(format!(
                "expected {} embeddings of non-zero width, got {} values with shape {:?}",
                crops.len(),
                values.len(),
                shape.as_ref()
            )));
        }
        self.dim = dim;

        Ok((0..crops.len())
            .map(|i| Embedding::normalise(values[i * dim..(i + 1) * dim].to_vec()))
            .collect())
    }
}

impl FaceEmbedder for ArcFaceEmbedder {
    fn name(&self) -> &str {
        &self.name
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&mut self, image: &RgbImage, detection: &Detection) -> Result<Embedding> {
        let crop = prepare_face(image, detection);
        self.run(std::slice::from_ref(&crop))?
            .pop()
            .ok_or(EmbedError::AlignmentFailed)
    }

    /// Batches every face in a frame into a single inference call, which is
    /// where most of the speed-up on group shots comes from (§19).
    fn embed_batch(&mut self, image: &RgbImage, detections: &[Detection]) -> Vec<Result<Embedding>> {
        if detections.is_empty() {
            return Vec::new();
        }

        let crops: Vec<RgbImage> = detections.iter().map(|d| prepare_face(image, d)).collect();
        match self.run(&crops) {
            Ok(embeddings) if embeddings.len() == detections.len() => {
                embeddings.into_iter().map(Ok).collect()
            }
            // A batch failure must not silently drop faces: report it per face
            // so the caller can mark each one and carry on.
            Ok(_) => detections
                .iter()
                .map(|_| Err(EmbedError::BadOutput("batch returned the wrong number of embeddings".into())))
                .collect(),
            Err(e) => detections
                .iter()
                .map(|_| Err(EmbedError::Runtime(e.to_string())))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tensor_layout_is_nchw_and_normalised() {
        let mut crop = RgbImage::new(ALIGNED_SIZE, ALIGNED_SIZE);
        crop.put_pixel(0, 0, image::Rgb([255, 0, 127]));
        let data = ArcFaceEmbedder::to_tensor(std::slice::from_ref(&crop));

        let plane = (ALIGNED_SIZE * ALIGNED_SIZE) as usize;
        assert_eq!(data.len(), 3 * plane);
        // Channels are separated, not interleaved.
        assert!((data[0] - 1.0).abs() < 1e-6, "R 255 maps to +1");
        assert!((data[plane] + 1.0).abs() < 1e-6, "G 0 maps to -1");
        assert!((data[2 * plane] - (127.0 - 127.5) / 127.5).abs() < 1e-6);
    }

    #[test]
    fn batching_lays_images_out_back_to_back() {
        let a = RgbImage::from_pixel(ALIGNED_SIZE, ALIGNED_SIZE, image::Rgb([255, 255, 255]));
        let b = RgbImage::from_pixel(ALIGNED_SIZE, ALIGNED_SIZE, image::Rgb([0, 0, 0]));
        let data = ArcFaceEmbedder::to_tensor(&[a, b]);

        let per_image = 3 * (ALIGNED_SIZE * ALIGNED_SIZE) as usize;
        assert_eq!(data.len(), 2 * per_image);
        assert!((data[0] - 1.0).abs() < 1e-6);
        assert!((data[per_image] + 1.0).abs() < 1e-6);
    }

    #[test]
    fn empty_batch_produces_no_tensor_data() {
        assert!(ArcFaceEmbedder::to_tensor(&[]).is_empty());
    }
}
