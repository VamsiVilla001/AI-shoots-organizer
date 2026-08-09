//! SCRFD face detector.
//!
//! SCRFD is an anchor-based single-shot detector that emits, per feature-map
//! stride, a confidence score, a box expressed as four distances from the
//! anchor centre, and (in the `bnkps` variants) five facial landmarks. This
//! module turns those raw tensors back into pixel coordinates.
//!
//! The output *order* of the ONNX graph is not guaranteed across exports, so
//! tensors are grouped by their channel count — 1 is a score, 4 is a box, 10
//! is a landmark set — which holds for every SCRFD export in circulation.

use std::path::Path;

use image::{imageops::FilterType, RgbImage};
use ort::session::Session;
use ort::value::Tensor;

use crate::runtime::{build_session, SessionConfig};
use crate::{non_max_suppression, Detection, DetectorConfig, FaceDetector, FaceError, Landmarks, Rect, Result};

/// SCRFD normalises with a fixed mean and scale rather than ImageNet statistics.
const PIXEL_MEAN: f32 = 127.5;
const PIXEL_SCALE: f32 = 128.0;

pub struct ScrfdDetector {
    session: Session,
    input_name: String,
    config: DetectorConfig,
    name: String,
}

/// Maps an image into the square the network expects, without distorting it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Letterbox {
    pub scale: f32,
    pub width: u32,
    pub height: u32,
}

/// SCRFD pads at the bottom-right only, so a detection maps back to the
/// original by dividing through by `scale` — no offset to undo.
pub(crate) fn letterbox_params(image_w: u32, image_h: u32, target: u32) -> Letterbox {
    let scale = (target as f32 / image_w as f32).min(target as f32 / image_h as f32);
    Letterbox {
        scale,
        width: ((image_w as f32 * scale).round() as u32).max(1),
        height: ((image_h as f32 * scale).round() as u32).max(1),
    }
}

impl ScrfdDetector {
    pub fn load(model_path: &Path, config: DetectorConfig, session_config: &SessionConfig) -> Result<Self> {
        let session = build_session(model_path, session_config)?;
        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| FaceError::BadOutput("model declares no inputs".into()))?;

        let name = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("scrfd")
            .to_string();

        Ok(Self { session, input_name, config, name })
    }

    pub fn config(&self) -> &DetectorConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: DetectorConfig) {
        self.config = config;
    }

    /// Builds the NCHW input tensor: resize into the letterbox, paste onto a
    /// zero canvas, then normalise.
    fn preprocess(&self, image: &RgbImage) -> (Vec<f32>, Letterbox, u32) {
        let target = self.config.input_size;
        let lb = letterbox_params(image.width(), image.height(), target);
        let resized = image::imageops::resize(image, lb.width, lb.height, FilterType::Triangle);

        let plane = (target * target) as usize;
        let mut tensor = vec![0.0f32; plane * 3];
        for y in 0..lb.height.min(target) {
            for x in 0..lb.width.min(target) {
                let px = resized.get_pixel(x, y);
                let offset = (y * target + x) as usize;
                for c in 0..3 {
                    tensor[c * plane + offset] = (px[c] as f32 - PIXEL_MEAN) / PIXEL_SCALE;
                }
            }
        }
        (tensor, lb, target)
    }
}

/// One decoded output tensor, flattened to `rows` × `channels`.
struct Plane<'a> {
    rows: usize,
    channels: usize,
    data: &'a [f32],
}

impl FaceDetector for ScrfdDetector {
    fn name(&self) -> &str {
        &self.name
    }

    fn detect(&mut self, image: &RgbImage) -> Result<Vec<Detection>> {
        if image.width() == 0 || image.height() == 0 {
            return Ok(Vec::new());
        }

        let (input, letterbox, target) = self.preprocess(image);
        let tensor = Tensor::from_array(([1usize, 3, target as usize, target as usize], input))
            .map_err(|e| FaceError::Runtime(e.to_string()))?;

        let outputs = self
            .session
            .run(ort::inputs![self.input_name.as_str() => tensor])
            .map_err(|e| FaceError::Runtime(e.to_string()))?;

        // Flatten every output to rows × channels.
        let mut planes: Vec<Plane<'_>> = Vec::with_capacity(outputs.len());
        for index in 0..outputs.len() {
            let (shape, data) = outputs[index]
                .try_extract_tensor::<f32>()
                .map_err(|e| FaceError::BadOutput(format!("output {index}: {e}")))?;
            let channels = shape.last().copied().unwrap_or(1).max(1) as usize;
            planes.push(Plane { rows: data.len() / channels, channels, data });
        }

        let mut scores: Vec<&Plane<'_>> = planes.iter().filter(|p| p.channels == 1).collect();
        let mut boxes: Vec<&Plane<'_>> = planes.iter().filter(|p| p.channels == 4).collect();
        let mut kps: Vec<&Plane<'_>> = planes.iter().filter(|p| p.channels == 10).collect();

        if scores.is_empty() || scores.len() != boxes.len() {
            return Err(FaceError::BadOutput(format!(
                "expected matching score and box outputs, got {} and {}",
                scores.len(),
                boxes.len()
            )));
        }

        // Largest feature map first: that is stride 8, then 16, then 32.
        scores.sort_by_key(|p| std::cmp::Reverse(p.rows));
        boxes.sort_by_key(|p| std::cmp::Reverse(p.rows));
        kps.sort_by_key(|p| std::cmp::Reverse(p.rows));
        let use_kps = kps.len() == scores.len();

        let mut detections = Vec::new();
        for (level, (score_plane, box_plane)) in scores.iter().zip(boxes.iter()).enumerate() {
            let stride = 8u32 << level; // 8, 16, 32, 64, 128
            let grid = (target / stride).max(1) as usize;
            let positions = grid * grid;
            if positions == 0 || score_plane.rows % positions != 0 {
                // A feature map that does not divide evenly means our stride
                // assumption is wrong for this model; skip rather than emit
                // nonsense boxes.
                tracing::warn!(stride, rows = score_plane.rows, "unexpected SCRFD feature map size");
                continue;
            }
            let anchors = score_plane.rows / positions;

            for row in 0..score_plane.rows {
                let score = score_plane.data[row];
                if score < self.config.score_threshold {
                    continue;
                }

                let position = row / anchors;
                let cx = ((position % grid) * stride as usize) as f32;
                let cy = ((position / grid) * stride as usize) as f32;

                let d = &box_plane.data[row * 4..row * 4 + 4];
                let bbox = Rect {
                    x1: (cx - d[0] * stride as f32) / letterbox.scale,
                    y1: (cy - d[1] * stride as f32) / letterbox.scale,
                    x2: (cx + d[2] * stride as f32) / letterbox.scale,
                    y2: (cy + d[3] * stride as f32) / letterbox.scale,
                };

                let landmarks = if use_kps {
                    let k = &kps[level].data[row * 10..row * 10 + 10];
                    let mut points: Landmarks = [(0.0, 0.0); 5];
                    for (i, point) in points.iter_mut().enumerate() {
                        *point = (
                            (cx + k[i * 2] * stride as f32) / letterbox.scale,
                            (cy + k[i * 2 + 1] * stride as f32) / letterbox.scale,
                        );
                    }
                    Some(points)
                } else {
                    None
                };

                detections.push(Detection { bbox, score, landmarks });
            }
        }

        Ok(non_max_suppression(detections, self.config.nms_threshold, self.config.max_faces))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letterbox_preserves_aspect_ratio() {
        let lb = letterbox_params(1920, 1080, 640);
        assert_eq!(lb.width, 640);
        assert_eq!(lb.height, 360);
        assert!((lb.scale - 640.0 / 1920.0).abs() < 1e-6);
    }

    #[test]
    fn letterbox_fits_tall_images_by_height() {
        let lb = letterbox_params(1080, 1920, 640);
        assert_eq!(lb.height, 640);
        assert_eq!(lb.width, 360);
    }

    #[test]
    fn letterbox_upscales_small_images_to_fill_the_input() {
        let lb = letterbox_params(320, 240, 640);
        assert!(lb.scale > 1.0);
        assert_eq!(lb.width, 640);
        assert_eq!(lb.height, 480);
    }

    #[test]
    fn a_square_image_maps_exactly() {
        let lb = letterbox_params(1000, 1000, 640);
        assert_eq!((lb.width, lb.height), (640, 640));
        assert!((lb.scale - 0.64).abs() < 1e-6);
    }

    /// Reproduces the decode arithmetic on hand-built values, so the mapping
    /// from anchor distances back to pixels is pinned without needing a model.
    #[test]
    fn anchor_distances_decode_to_pixel_boxes() {
        let stride = 8.0_f32;
        let scale = 0.5_f32; // the image was halved to fit the network input
        let (cx, cy) = (80.0_f32, 40.0_f32);
        // Distances are in stride units: 2 strides left/up, 2 right/down.
        let d = [2.0_f32, 2.0, 2.0, 2.0];

        let bbox = Rect {
            x1: (cx - d[0] * stride) / scale,
            y1: (cy - d[1] * stride) / scale,
            x2: (cx + d[2] * stride) / scale,
            y2: (cy + d[3] * stride) / scale,
        };

        assert_eq!(bbox.x1, 128.0);
        assert_eq!(bbox.y1, 48.0);
        assert_eq!(bbox.x2, 192.0);
        assert_eq!(bbox.y2, 112.0);
        assert_eq!(bbox.width(), 64.0);
    }

    #[test]
    fn feature_map_positions_match_the_grid() {
        // A 640 input at stride 32 gives a 20x20 grid with 2 anchors each.
        let target = 640u32;
        let stride = 32u32;
        let grid = (target / stride) as usize;
        assert_eq!(grid, 20);

        let anchors = 2usize;
        let rows = grid * grid * anchors;
        assert_eq!(rows, 800);

        // The last row must land on the bottom-right cell.
        let position = (rows - 1) / anchors;
        assert_eq!((position % grid) * stride as usize, 608);
        assert_eq!((position / grid) * stride as usize, 608);
    }
}
