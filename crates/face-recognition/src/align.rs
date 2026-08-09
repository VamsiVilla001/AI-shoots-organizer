//! Face alignment (§4).
//!
//! An embedding model only compares faces meaningfully if they arrive in the
//! same pose, at the same scale, in the same place in the frame. Given the five
//! landmarks the detector returns, we solve for the rotation, uniform scale and
//! translation that best carries them onto a fixed template, then resample the
//! source image through that transform.

use image::RgbImage;

use teo_face_detection::{Landmarks, Rect};

/// The canonical ArcFace 112×112 landmark template: left eye, right eye, nose,
/// left mouth corner, right mouth corner.
pub const ARCFACE_TEMPLATE: [(f32, f32); 5] = [
    (38.2946, 51.6963),
    (73.5318, 51.5014),
    (56.0252, 71.7366),
    (41.5493, 92.3655),
    (70.7299, 92.2041),
];

/// The side length the template is defined against.
pub const ALIGNED_SIZE: u32 = 112;

/// A 2-D similarity transform: rotation and uniform scale in `m`, offset in `t`.
///
/// Stored as the four numbers that actually vary — a similarity has only two
/// free parameters in its linear part (`sc = s·cosθ`, `ss = s·sinθ`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Similarity {
    pub sc: f32,
    pub ss: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Similarity {
    pub const IDENTITY: Similarity = Similarity { sc: 1.0, ss: 0.0, tx: 0.0, ty: 0.0 };

    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (self.sc * x - self.ss * y + self.tx, self.ss * x + self.sc * y + self.ty)
    }

    /// The reverse mapping, used to walk output pixels back to source pixels.
    /// Returns `None` for a degenerate transform (all landmarks coincident).
    pub fn inverse(&self) -> Option<Similarity> {
        let det = self.sc * self.sc + self.ss * self.ss;
        if det.abs() < 1e-12 {
            return None;
        }
        let (isc, iss) = (self.sc / det, -self.ss / det);
        Some(Similarity {
            sc: isc,
            ss: iss,
            tx: -(isc * self.tx - iss * self.ty),
            ty: -(iss * self.tx + isc * self.ty),
        })
    }

    /// The uniform scale factor this transform applies.
    pub fn scale(&self) -> f32 {
        (self.sc * self.sc + self.ss * self.ss).sqrt()
    }
}

/// Least-squares similarity transform carrying `src` onto `dst`.
///
/// This is the closed form of the 2-D Procrustes problem restricted to
/// rotation, uniform scale and translation. Restricting it that way is the
/// point: a full affine fit would happily shear a three-quarter profile into
/// the template and destroy exactly the geometry the embedding depends on.
pub fn estimate_similarity(src: &[(f32, f32)], dst: &[(f32, f32)]) -> Option<Similarity> {
    let n = src.len().min(dst.len());
    if n < 2 {
        return None;
    }
    let inv_n = 1.0 / n as f32;

    let (mut sx, mut sy, mut dx, mut dy) = (0.0f32, 0.0, 0.0, 0.0);
    for i in 0..n {
        sx += src[i].0;
        sy += src[i].1;
        dx += dst[i].0;
        dy += dst[i].1;
    }
    let (mean_sx, mean_sy) = (sx * inv_n, sy * inv_n);
    let (mean_dx, mean_dy) = (dx * inv_n, dy * inv_n);

    let (mut norm, mut a, mut b) = (0.0f32, 0.0, 0.0);
    for i in 0..n {
        let (ux, uy) = (src[i].0 - mean_sx, src[i].1 - mean_sy);
        let (vx, vy) = (dst[i].0 - mean_dx, dst[i].1 - mean_dy);
        norm += ux * ux + uy * uy;
        a += ux * vx + uy * vy; // scale · cos θ, once divided through
        b += ux * vy - uy * vx; // scale · sin θ
    }
    if norm < 1e-12 {
        return None;
    }

    let sc = a / norm;
    let ss = b / norm;
    Some(Similarity {
        sc,
        ss,
        tx: mean_dx - (sc * mean_sx - ss * mean_sy),
        ty: mean_dy - (ss * mean_sx + sc * mean_sy),
    })
}

/// Aligns a face into a 112×112 crop using its landmarks.
pub fn align_face(image: &RgbImage, landmarks: &Landmarks) -> Option<RgbImage> {
    let transform = estimate_similarity(landmarks, &ARCFACE_TEMPLATE)?;
    let inverse = transform.inverse()?;
    Some(warp(image, &inverse, ALIGNED_SIZE, ALIGNED_SIZE))
}

/// Alignment fallback for detectors that return no landmarks: take the box,
/// pad it out to roughly the framing the template expects, and square it off.
/// Less accurate than landmark alignment, but far better than nothing.
pub fn align_from_bbox(image: &RgbImage, bbox: &Rect) -> RgbImage {
    let padded = bbox.expanded(0.15, image.width(), image.height());
    let (cx, cy) = padded.center();
    let side = padded.width().max(padded.height()).max(1.0);

    let scale = ALIGNED_SIZE as f32 / side;
    let transform = Similarity {
        sc: scale,
        ss: 0.0,
        tx: ALIGNED_SIZE as f32 / 2.0 - scale * cx,
        ty: ALIGNED_SIZE as f32 / 2.0 - scale * cy,
    };

    match transform.inverse() {
        Some(inverse) => warp(image, &inverse, ALIGNED_SIZE, ALIGNED_SIZE),
        None => RgbImage::new(ALIGNED_SIZE, ALIGNED_SIZE),
    }
}

/// Resamples `image` into a `width`×`height` output, where `inverse` maps
/// output coordinates back to source coordinates.
fn warp(image: &RgbImage, inverse: &Similarity, width: u32, height: u32) -> RgbImage {
    let mut out = RgbImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let (sx, sy) = inverse.apply(x as f32 + 0.5, y as f32 + 0.5);
            out.put_pixel(x, y, sample_bilinear(image, sx - 0.5, sy - 0.5));
        }
    }
    out
}

/// Bilinear sample with edge clamping, so a face at the border of the frame
/// still aligns instead of picking up a black margin.
fn sample_bilinear(image: &RgbImage, x: f32, y: f32) -> image::Rgb<u8> {
    let (w, h) = (image.width() as i64, image.height() as i64);
    if w == 0 || h == 0 {
        return image::Rgb([0, 0, 0]);
    }

    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;

    let clamp = |v: i64, max: i64| v.clamp(0, max - 1) as u32;
    let (x0i, y0i) = (x0 as i64, y0 as i64);
    let px = [
        image.get_pixel(clamp(x0i, w), clamp(y0i, h)),
        image.get_pixel(clamp(x0i + 1, w), clamp(y0i, h)),
        image.get_pixel(clamp(x0i, w), clamp(y0i + 1, h)),
        image.get_pixel(clamp(x0i + 1, w), clamp(y0i + 1, h)),
    ];

    let mut out = [0u8; 3];
    for (c, value) in out.iter_mut().enumerate() {
        let top = px[0][c] as f32 * (1.0 - fx) + px[1][c] as f32 * fx;
        let bottom = px[2][c] as f32 * (1.0 - fx) + px[3][c] as f32 * fx;
        *value = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
    }
    image::Rgb(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn recovers_a_known_rotation_scale_and_offset() {
        // Build a transform, push points through it, and check we solve back.
        let truth = Similarity { sc: 1.5_f32 * 0.8, ss: 1.5 * 0.6, tx: 12.0, ty: -7.0 };
        let src = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0), (5.0, 5.0)];
        let dst: Vec<(f32, f32)> = src.iter().map(|&(x, y)| truth.apply(x, y)).collect();

        let solved = estimate_similarity(&src, &dst).unwrap();
        assert!((solved.sc - truth.sc).abs() < 1e-4, "sc {} vs {}", solved.sc, truth.sc);
        assert!((solved.ss - truth.ss).abs() < 1e-4);
        assert!((solved.tx - truth.tx).abs() < 1e-3);
        assert!((solved.ty - truth.ty).abs() < 1e-3);
        assert!((solved.scale() - 1.5).abs() < 1e-4);
    }

    #[test]
    fn inverse_undoes_the_transform() {
        let t = Similarity { sc: 0.7, ss: -0.35, tx: 40.0, ty: 11.0 };
        let inverse = t.inverse().unwrap();
        for &(x, y) in &[(0.0_f32, 0.0_f32), (13.0, 91.0), (-4.0, 6.5)] {
            let (fx, fy) = t.apply(x, y);
            let (bx, by) = inverse.apply(fx, fy);
            assert!((bx - x).abs() < 1e-3, "{bx} vs {x}");
            assert!((by - y).abs() < 1e-3, "{by} vs {y}");
        }
    }

    #[test]
    fn degenerate_input_is_rejected_rather_than_dividing_by_zero() {
        let coincident = [(5.0_f32, 5.0_f32); 5];
        assert!(estimate_similarity(&coincident, &ARCFACE_TEMPLATE).is_none());
        assert!(estimate_similarity(&[(0.0, 0.0)], &ARCFACE_TEMPLATE).is_none());
        assert!(Similarity { sc: 0.0, ss: 0.0, tx: 1.0, ty: 1.0 }.inverse().is_none());
    }

    #[test]
    fn landmarks_on_the_template_align_to_the_template() {
        // A face whose landmarks already sit exactly on the template should
        // come back essentially unchanged.
        let mut image = RgbImage::new(112, 112);
        for (x, y) in ARCFACE_TEMPLATE {
            image.put_pixel(x as u32, y as u32, Rgb([255, 0, 0]));
        }
        let aligned = align_face(&image, &ARCFACE_TEMPLATE).unwrap();
        assert_eq!(aligned.dimensions(), (112, 112));

        for (x, y) in ARCFACE_TEMPLATE {
            let px = aligned.get_pixel(x as u32, y as u32);
            assert!(px[0] > 100, "landmark at ({x},{y}) lost its marker: {px:?}");
        }
    }

    #[test]
    fn alignment_normalises_a_scaled_and_shifted_face() {
        // The same landmark geometry, twice the size and offset in the frame,
        // must land back on the template.
        let scaled: Landmarks = {
            let mut out = [(0.0, 0.0); 5];
            for (i, (x, y)) in ARCFACE_TEMPLATE.iter().enumerate() {
                out[i] = (x * 2.0 + 40.0, y * 2.0 + 25.0);
            }
            out
        };

        let transform = estimate_similarity(&scaled, &ARCFACE_TEMPLATE).unwrap();
        for (i, &(x, y)) in scaled.iter().enumerate() {
            let (ax, ay) = transform.apply(x, y);
            assert!((ax - ARCFACE_TEMPLATE[i].0).abs() < 1e-2);
            assert!((ay - ARCFACE_TEMPLATE[i].1).abs() < 1e-2);
        }
        assert!((transform.scale() - 0.5).abs() < 1e-4);
    }

    #[test]
    fn bbox_fallback_produces_a_square_crop() {
        let mut image = RgbImage::new(400, 300);
        for y in 100..200 {
            for x in 150..250 {
                image.put_pixel(x, y, Rgb([0, 200, 0]));
            }
        }
        let crop = align_from_bbox(&image, &Rect { x1: 150.0, y1: 100.0, x2: 250.0, y2: 200.0 });
        assert_eq!(crop.dimensions(), (112, 112));
        // The centre of the crop must come from inside the green square.
        assert!(crop.get_pixel(56, 56)[1] > 150);
    }

    #[test]
    fn bilinear_sampling_clamps_at_the_edges() {
        let mut image = RgbImage::new(2, 2);
        image.put_pixel(0, 0, Rgb([10, 10, 10]));
        image.put_pixel(1, 0, Rgb([20, 20, 20]));
        image.put_pixel(0, 1, Rgb([30, 30, 30]));
        image.put_pixel(1, 1, Rgb([40, 40, 40]));

        assert_eq!(sample_bilinear(&image, -5.0, -5.0), Rgb([10, 10, 10]));
        assert_eq!(sample_bilinear(&image, 99.0, 99.0), Rgb([40, 40, 40]));
        // Halfway between all four is their mean.
        assert_eq!(sample_bilinear(&image, 0.5, 0.5), Rgb([25, 25, 25]));
    }
}
