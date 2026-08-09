//! Face detection behind a swappable interface (§14).
//!
//! The application talks to [`FaceDetector`], never to a specific model. The
//! bundled implementation is SCRFD, but anything that can turn an image into
//! boxes and five landmarks can replace it without touching the rest of the
//! product.

pub mod nms;
pub mod runtime;
pub mod scrfd;

use image::RgbImage;

pub use nms::non_max_suppression;
pub use runtime::{available_accelerators, Accelerator, SessionConfig};
pub use scrfd::ScrfdDetector;

#[derive(Debug, thiserror::Error)]
pub enum FaceError {
    #[error("model file not found: {0}")]
    ModelMissing(String),
    #[error("ONNX Runtime error: {0}")]
    Runtime(String),
    #[error("unexpected model output: {0}")]
    BadOutput(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, FaceError>;

/// An axis-aligned box in pixel coordinates of the image it was found in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl Rect {
    pub fn width(&self) -> f32 {
        (self.x2 - self.x1).max(0.0)
    }

    pub fn height(&self) -> f32 {
        (self.y2 - self.y1).max(0.0)
    }

    pub fn area(&self) -> f32 {
        self.width() * self.height()
    }

    pub fn center(&self) -> (f32, f32) {
        ((self.x1 + self.x2) / 2.0, (self.y1 + self.y2) / 2.0)
    }

    /// Intersection over union, the overlap measure NMS is built on.
    pub fn iou(&self, other: &Rect) -> f32 {
        let x1 = self.x1.max(other.x1);
        let y1 = self.y1.max(other.y1);
        let x2 = self.x2.min(other.x2);
        let y2 = self.y2.min(other.y2);
        let intersection = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
        let union = self.area() + other.area() - intersection;
        if union <= 0.0 {
            0.0
        } else {
            intersection / union
        }
    }

    /// Grows the box by `factor` on every side, clamped to the image.
    pub fn expanded(&self, factor: f32, width: u32, height: u32) -> Rect {
        let dx = self.width() * factor;
        let dy = self.height() * factor;
        Rect {
            x1: (self.x1 - dx).max(0.0),
            y1: (self.y1 - dy).max(0.0),
            x2: (self.x2 + dx).min(width as f32),
            y2: (self.y2 + dy).min(height as f32),
        }
    }

    /// Fractions of the frame, which is how bounding boxes are persisted so
    /// they stay valid against a thumbnail as well as the original.
    pub fn normalised(&self, width: u32, height: u32) -> (f64, f64, f64, f64) {
        let (w, h) = (width.max(1) as f64, height.max(1) as f64);
        (
            (self.x1 as f64 / w).clamp(0.0, 1.0),
            (self.y1 as f64 / h).clamp(0.0, 1.0),
            (self.width() as f64 / w).clamp(0.0, 1.0),
            (self.height() as f64 / h).clamp(0.0, 1.0),
        )
    }
}

/// The five landmarks ArcFace alignment expects, in pixel coordinates:
/// left eye, right eye, nose, left mouth corner, right mouth corner.
pub type Landmarks = [(f32, f32); 5];

#[derive(Debug, Clone)]
pub struct Detection {
    pub bbox: Rect,
    pub score: f32,
    pub landmarks: Option<Landmarks>,
}

impl Detection {
    /// A rough usability score in 0..1, combining detector confidence with how
    /// much of the frame the face occupies. Small background faces are real but
    /// make poor library samples, so this is used to pick cover images and to
    /// weight matches — never to discard a detection.
    pub fn quality(&self, image_width: u32, image_height: u32) -> f64 {
        let frame_area = (image_width as f32 * image_height as f32).max(1.0);
        // A face filling 5% of the frame is already a good portrait crop.
        let relative_size = (self.bbox.area() / frame_area / 0.05).clamp(0.0, 1.0);
        let confidence = self.score.clamp(0.0, 1.0);
        (0.6 * confidence + 0.4 * relative_size) as f64
    }
}

#[derive(Debug, Clone)]
pub struct DetectorConfig {
    /// Detections below this score are dropped.
    pub score_threshold: f32,
    /// Boxes overlapping more than this are treated as the same face.
    pub nms_threshold: f32,
    /// Longest edge the image is resized to before inference. Larger finds
    /// smaller faces at proportionally more cost (§19).
    pub input_size: u32,
    /// Guards against a pathological frame producing thousands of boxes.
    pub max_faces: usize,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            score_threshold: 0.5,
            nms_threshold: 0.4,
            input_size: 640,
            max_faces: 64,
        }
    }
}

/// Anything that can find faces in an image.
pub trait FaceDetector: Send {
    fn detect(&mut self, image: &RgbImage) -> Result<Vec<Detection>>;

    /// Identifies the model, for logging and the Settings screen.
    fn name(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x1: f32, y1: f32, x2: f32, y2: f32) -> Rect {
        Rect { x1, y1, x2, y2 }
    }

    #[test]
    fn iou_of_identical_boxes_is_one() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        assert!((a.iou(&a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn iou_of_disjoint_boxes_is_zero() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(20.0, 20.0, 30.0, 30.0);
        assert_eq!(a.iou(&b), 0.0);
    }

    #[test]
    fn iou_of_half_overlap() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(5.0, 0.0, 15.0, 10.0);
        // 50 intersection over 150 union.
        assert!((a.iou(&b) - 1.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn normalised_box_is_a_fraction_of_the_frame() {
        let r = rect(100.0, 50.0, 300.0, 250.0);
        let (x, y, w, h) = r.normalised(1000, 500);
        assert!((x - 0.1).abs() < 1e-9);
        assert!((y - 0.1).abs() < 1e-9);
        assert!((w - 0.2).abs() < 1e-9);
        assert!((h - 0.4).abs() < 1e-9);
    }

    #[test]
    fn expansion_clamps_to_the_image() {
        let r = rect(5.0, 5.0, 15.0, 15.0);
        let e = r.expanded(1.0, 20, 20);
        assert_eq!(e.x1, 0.0);
        assert_eq!(e.y1, 0.0);
        assert_eq!(e.x2, 20.0);
        assert_eq!(e.y2, 20.0);
    }

    #[test]
    fn quality_rewards_large_confident_faces() {
        let big = Detection {
            bbox: rect(0.0, 0.0, 400.0, 400.0),
            score: 0.99,
            landmarks: None,
        };
        let small = Detection {
            bbox: rect(0.0, 0.0, 20.0, 20.0),
            score: 0.55,
            landmarks: None,
        };
        assert!(big.quality(1920, 1080) > small.quality(1920, 1080));
        assert!(big.quality(1920, 1080) <= 1.0);
    }
}
