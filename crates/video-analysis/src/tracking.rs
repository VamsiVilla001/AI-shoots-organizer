//! Optional OpenCV optical-flow tracking between already sampled frames.
//!
//! Tracking never decides identity. It only proposes where a previously
//! embedded face moved; the desktop pipeline requires a fresh ArcFace match
//! before accepting a proposal that SCRFD missed.

use image::RgbImage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackProposal {
    pub bbox: TrackBox,
    pub confidence: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackingBackend {
    OpenCv,
    Disabled,
}

pub fn backend() -> TrackingBackend {
    if cfg!(feature = "opencv-tracking") {
        TrackingBackend::OpenCv
    } else {
        TrackingBackend::Disabled
    }
}

#[cfg(feature = "opencv-tracking")]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct NativeTrackedBox {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    confidence: f32,
    valid: i32,
}

#[cfg(feature = "opencv-tracking")]
extern "C" {
    fn teo_opencv_track_boxes(
        previous_rgb: *const u8,
        current_rgb: *const u8,
        width: i32,
        height: i32,
        boxes: *const f32,
        box_count: usize,
        output: *mut NativeTrackedBox,
    ) -> i32;
}

/// Tracks every source box into `current`. Each output slot corresponds to the
/// source box at the same index; `None` is the safe result for a cut, blur, or
/// insufficient visual features.
pub fn track_boxes(previous: &RgbImage, current: &RgbImage, boxes: &[TrackBox]) -> Vec<Option<TrackProposal>> {
    if previous.dimensions() != current.dimensions() || boxes.is_empty() {
        return vec![None; boxes.len()];
    }

    track_boxes_impl(previous, current, boxes)
}

#[cfg(not(feature = "opencv-tracking"))]
fn track_boxes_impl(_previous: &RgbImage, _current: &RgbImage, boxes: &[TrackBox]) -> Vec<Option<TrackProposal>> {
    vec![None; boxes.len()]
}

#[cfg(feature = "opencv-tracking")]
fn track_boxes_impl(previous: &RgbImage, current: &RgbImage, boxes: &[TrackBox]) -> Vec<Option<TrackProposal>> {
    let (width, height) = previous.dimensions();
    let Ok(width) = i32::try_from(width) else {
        return vec![None; boxes.len()];
    };
    let Ok(height) = i32::try_from(height) else {
        return vec![None; boxes.len()];
    };
    let packed: Vec<f32> = boxes
        .iter()
        .flat_map(|bbox| [bbox.x, bbox.y, bbox.width, bbox.height])
        .collect();
    let mut native = vec![NativeTrackedBox::default(); boxes.len()];

    // SAFETY: all pointers refer to contiguous buffers that outlive the call;
    // OpenCV receives the exact common dimensions checked above and the native
    // bridge catches C++ exceptions before they cross the ABI boundary.
    let status = unsafe {
        teo_opencv_track_boxes(
            previous.as_raw().as_ptr(),
            current.as_raw().as_ptr(),
            width,
            height,
            packed.as_ptr(),
            boxes.len(),
            native.as_mut_ptr(),
        )
    };
    if status != 0 {
        tracing::warn!(
            status,
            "OpenCV optical-flow tracking failed; keeping detector-only results"
        );
        return vec![None; boxes.len()];
    }

    native
        .into_iter()
        .map(|tracked| {
            (tracked.valid != 0 && tracked.confidence.is_finite()).then_some(TrackProposal {
                bbox: TrackBox {
                    x: tracked.x,
                    y: tracked.y,
                    width: tracked.width,
                    height: tracked.height,
                },
                confidence: tracked.confidence.clamp(0.0, 1.0),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mismatched_frame_sizes_fail_closed() {
        let previous = RgbImage::new(80, 60);
        let current = RgbImage::new(40, 30);
        let result = track_boxes(
            &previous,
            &current,
            &[TrackBox {
                x: 0.1,
                y: 0.1,
                width: 0.2,
                height: 0.2,
            }],
        );
        assert_eq!(result, vec![None]);
    }

    #[cfg(feature = "opencv-tracking")]
    #[test]
    fn optical_flow_follows_a_translated_textured_face_region() {
        let mut previous = RgbImage::new(160, 120);
        let mut current = RgbImage::new(160, 120);
        for y in 30..80 {
            for x in 40..90 {
                let pattern = if (x / 5 + y / 5) % 2 == 0 { 240 } else { 20 };
                previous.put_pixel(x, y, image::Rgb([pattern, 255 - pattern, pattern]));
                current.put_pixel(x + 8, y + 4, image::Rgb([pattern, 255 - pattern, pattern]));
            }
        }
        let tracked = track_boxes(
            &previous,
            &current,
            &[TrackBox {
                x: 0.25,
                y: 0.25,
                width: 0.3125,
                height: 0.416_666_66,
            }],
        )[0]
        .expect("the textured box should track");
        assert!((tracked.bbox.x - 0.30).abs() < 0.02, "x={}", tracked.bbox.x);
        assert!((tracked.bbox.y - 0.2833).abs() < 0.02, "y={}", tracked.bbox.y);
        assert!(tracked.confidence >= 0.5, "confidence={}", tracked.confidence);
    }

    #[cfg(feature = "opencv-tracking")]
    #[test]
    fn tracked_boxes_remain_inside_the_frame() {
        let mut previous = RgbImage::new(100, 80);
        let mut current = RgbImage::new(100, 80);
        for y in 25..65 {
            for x in 60..95 {
                let pattern = if (x / 4 + y / 4) % 2 == 0 { 245 } else { 15 };
                previous.put_pixel(x, y, image::Rgb([pattern, pattern, 255 - pattern]));
                if x + 4 < 100 {
                    current.put_pixel(x + 4, y, image::Rgb([pattern, pattern, 255 - pattern]));
                }
            }
        }
        let tracked = track_boxes(
            &previous,
            &current,
            &[TrackBox {
                x: 0.6,
                y: 0.3125,
                width: 0.35,
                height: 0.5,
            }],
        )[0]
            .expect("the edge box should still track");
        assert!(tracked.bbox.x + tracked.bbox.width <= 1.0);
        assert!(tracked.bbox.y + tracked.bbox.height <= 1.0);
    }
}
