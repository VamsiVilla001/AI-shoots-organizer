//! Exercises a real ONNX model end to end.
//!
//! Ignored by default: the model weights are fetched, not committed, so a
//! clean checkout has nothing to run against. Run it deliberately with
//!
//! ```text
//! cargo test -p teo-face-recognition --test real_model -- --ignored --nocapture
//! ```
//!
//! Its job is to cover the two things unit tests cannot: that batching really
//! produces one correct embedding per face, and that ONNX Runtime's
//! per-inference batch-shape warning is actually suppressed rather than merely
//! matched by a predicate.

use std::path::PathBuf;

use image::RgbImage;
use teo_face_detection::{Accelerator, Detection, Rect, SessionConfig};
use teo_face_recognition::{ArcFaceEmbedder, FaceEmbedder};

/// Where the fetch scripts install the models.
fn model_dir() -> Option<PathBuf> {
    let dir = if cfg!(windows) {
        PathBuf::from(std::env::var("APPDATA").ok()?).join("com.teorganiser.desktop/models")
    } else {
        PathBuf::from(std::env::var("HOME").ok()?)
            .join("Library/Application Support/com.teorganiser.desktop/models")
    };
    dir.is_dir().then_some(dir)
}

fn find_embedder() -> Option<PathBuf> {
    let dir = model_dir()?;
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains("w600k") || n.contains("arcface"))
        })
}

fn face_at(x: f32, y: f32) -> Detection {
    Detection {
        bbox: Rect { x1: x, y1: y, x2: x + 100.0, y2: y + 100.0 },
        score: 0.9,
        landmarks: None,
    }
}

#[test]
#[ignore = "needs the fetched ONNX models"]
fn batched_embedding_is_correct_and_quiet() {
    let Some(model) = find_embedder() else {
        panic!("no embedding model found — run scripts/fetch-models.ps1 first");
    };

    // Print ORT's logging to stderr so a suppressed message is visibly absent
    // and an unsuppressed one is visibly present.
    let _ = tracing_subscriber::fmt().with_max_level(tracing::Level::TRACE).try_init();

    let mut embedder = ArcFaceEmbedder::load(&model, &SessionConfig::default()).expect("failed to load model");

    // A frame with several distinct faces, embedded as one batch — the exact
    // situation that used to emit a warning per photo.
    let mut image = RgbImage::new(640, 480);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        *pixel = image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8]);
    }
    let detections = vec![face_at(10.0, 10.0), face_at(200.0, 60.0), face_at(400.0, 120.0)];

    let embeddings = embedder.embed_batch(&image, &detections);
    assert_eq!(embeddings.len(), 3, "one embedding per face");

    for (i, result) in embeddings.iter().enumerate() {
        let embedding = result.as_ref().unwrap_or_else(|e| panic!("face {i} failed: {e}"));
        assert_eq!(embedding.dim(), embedder.dim());
        let norm: f32 = embedding.as_slice().iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "face {i} embedding is not unit length: {norm}");
    }

    // Different crops of a gradient must not collapse to the same vector —
    // that would mean the batch was filled from one face.
    let a = embeddings[0].as_ref().unwrap();
    let b = embeddings[2].as_ref().unwrap();
    assert!(
        a.similarity(b) < 0.999,
        "distinct crops produced near-identical embeddings; the batch is not being filled per face"
    );

    println!("\n--- if no ORT 'does not match actual shape' lines appear above, the fix works ---");
}

/// DirectML rejects a variable batch outright rather than falling back, which
/// once made every face in a shoot fail. The embedder now chunks to whatever
/// the active provider accepts, so asking for more faces than that must still
/// return one usable embedding per face.
#[test]
#[ignore = "needs the fetched ONNX models"]
fn every_provider_embeds_a_whole_frame_correctly() {
    let Some(model) = find_embedder() else {
        panic!("no embedding model found — run scripts/fetch-models.ps1 first");
    };

    let mut image = RgbImage::new(1280, 720);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        *pixel = image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8]);
    }
    // Deliberately more faces than the GPU batch limit.
    let detections: Vec<Detection> = (0..9).map(|i| face_at(20.0 + i as f32 * 130.0, 40.0)).collect();

    for accelerator in [Accelerator::Cpu, Accelerator::Auto] {
        let config = SessionConfig { accelerator, ..SessionConfig::default() };
        let mut embedder = match ArcFaceEmbedder::load(&model, &config) {
            Ok(e) => e,
            Err(e) => {
                println!("{accelerator:?}: unavailable ({e}) — skipping");
                continue;
            }
        };

        let embeddings = embedder.embed_batch(&image, &detections);
        assert_eq!(embeddings.len(), detections.len(), "{accelerator:?} dropped faces");
        for (i, result) in embeddings.iter().enumerate() {
            let embedding = result
                .as_ref()
                .unwrap_or_else(|e| panic!("{accelerator:?} failed on face {i}: {e}"));
            let norm: f32 = embedding.as_slice().iter().map(|v| v * v).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-3, "{accelerator:?} face {i} not unit length");
        }
        println!("{accelerator:?}: {} faces embedded, max_batch = {}", embeddings.len(), embedder.max_batch());
    }
}
