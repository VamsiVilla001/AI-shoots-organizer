//! Non-maximum suppression.
//!
//! Anchor-based detectors fire several times on the same face; NMS keeps the
//! most confident box and discards everything that overlaps it too much.

use crate::Detection;

/// Returns detections sorted by descending score with overlapping duplicates
/// removed. `iou_threshold` is the overlap above which two boxes are treated
/// as the same face.
pub fn non_max_suppression(mut detections: Vec<Detection>, iou_threshold: f32, max_faces: usize) -> Vec<Detection> {
    detections.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    let mut kept: Vec<Detection> = Vec::new();
    for candidate in detections {
        if kept.len() >= max_faces {
            break;
        }
        if kept.iter().any(|k| k.bbox.iou(&candidate.bbox) > iou_threshold) {
            continue;
        }
        kept.push(candidate);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rect;

    fn detection(x1: f32, y1: f32, x2: f32, y2: f32, score: f32) -> Detection {
        Detection { bbox: Rect { x1, y1, x2, y2 }, score, landmarks: None }
    }

    #[test]
    fn collapses_duplicates_and_keeps_the_best() {
        let input = vec![
            detection(0.0, 0.0, 10.0, 10.0, 0.80),
            detection(1.0, 1.0, 11.0, 11.0, 0.95), // same face, more confident
            detection(50.0, 50.0, 60.0, 60.0, 0.70), // a different face
        ];
        let kept = non_max_suppression(input, 0.4, 64);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].score, 0.95);
        assert_eq!(kept[1].score, 0.70);
    }

    #[test]
    fn keeps_faces_that_merely_touch() {
        let input = vec![
            detection(0.0, 0.0, 10.0, 10.0, 0.9),
            detection(9.0, 0.0, 19.0, 10.0, 0.9),
        ];
        assert_eq!(non_max_suppression(input, 0.4, 64).len(), 2);
    }

    #[test]
    fn respects_the_face_cap() {
        let input = (0..100)
            .map(|i| detection(i as f32 * 100.0, 0.0, i as f32 * 100.0 + 10.0, 10.0, 0.9))
            .collect();
        assert_eq!(non_max_suppression(input, 0.4, 8).len(), 8);
    }

    #[test]
    fn empty_input_is_fine() {
        assert!(non_max_suppression(Vec::new(), 0.4, 64).is_empty());
    }
}
