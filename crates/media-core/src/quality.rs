//! Fast, local photo-quality signals used for culling and duplicate ranking.
//!
//! These deliberately analyse the cached thumbnail rather than reading a large
//! original again. The scores are editorial hints, not claims about aesthetics.

use image::{imageops::FilterType, GrayImage, RgbImage};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhotoQuality {
    pub overall: f64,
    pub sharpness: f64,
    pub exposure: f64,
    pub perceptual_hash: u64,
}

/// Measures focus detail and exposure, and creates a 64-bit difference hash.
pub fn analyse(image: &RgbImage) -> PhotoQuality {
    let gray = image::DynamicImage::ImageRgb8(image.clone()).into_luma8();
    let working = image::imageops::resize(&gray, 160, 160, FilterType::Triangle);
    let sharpness = sharpness_score(&working);
    let exposure = exposure_score(&working);

    PhotoQuality {
        overall: (sharpness * 0.75 + exposure * 0.25).clamp(0.0, 1.0),
        sharpness,
        exposure,
        perceptual_hash: difference_hash(&gray),
    }
}

fn sharpness_score(image: &GrayImage) -> f64 {
    if image.width() < 3 || image.height() < 3 {
        return 0.0;
    }

    let mut count = 0.0;
    let mut mean = 0.0;
    let mut m2 = 0.0;
    for y in 1..image.height() - 1 {
        for x in 1..image.width() - 1 {
            let centre = f64::from(image.get_pixel(x, y)[0]);
            let laplacian = f64::from(image.get_pixel(x - 1, y)[0])
                + f64::from(image.get_pixel(x + 1, y)[0])
                + f64::from(image.get_pixel(x, y - 1)[0])
                + f64::from(image.get_pixel(x, y + 1)[0])
                - 4.0 * centre;
            count += 1.0;
            let delta = laplacian - mean;
            mean += delta / count;
            m2 += delta * (laplacian - mean);
        }
    }

    let variance = if count > 1.0 { m2 / (count - 1.0) } else { 0.0 };
    (1.0 - (-variance / 650.0).exp()).clamp(0.0, 1.0)
}

fn exposure_score(image: &GrayImage) -> f64 {
    if image.is_empty() {
        return 0.0;
    }

    let count = f64::from(image.width()) * f64::from(image.height());
    let mut total = 0_u64;
    let mut clipped = 0_u64;
    for pixel in image.pixels() {
        let value = u64::from(pixel[0]);
        total += value;
        if value <= 5 || value >= 250 {
            clipped += 1;
        }
    }

    let mean = total as f64 / count / 255.0;
    let centred = (1.0 - (mean - 0.5).abs() / 0.5).clamp(0.0, 1.0);
    let clipping_penalty = 1.0 - (clipped as f64 / count).min(0.8);
    (centred * clipping_penalty).clamp(0.0, 1.0)
}

fn difference_hash(image: &GrayImage) -> u64 {
    let small = image::imageops::resize(image, 9, 8, FilterType::Triangle);
    let mut hash = 0_u64;
    for y in 0..8 {
        for x in 0..8 {
            hash <<= 1;
            if small.get_pixel(x, y)[0] > small.get_pixel(x + 1, y)[0] {
                hash |= 1;
            }
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn detail_scores_higher_than_a_flat_frame() {
        let flat = RgbImage::from_pixel(200, 200, Rgb([128, 128, 128]));
        let detailed = RgbImage::from_fn(200, 200, |x, y| {
            if (x / 4 + y / 4) % 2 == 0 {
                Rgb([20, 20, 20])
            } else {
                Rgb([235, 235, 235])
            }
        });

        assert!(analyse(&detailed).sharpness > analyse(&flat).sharpness);
    }

    #[test]
    fn balanced_exposure_beats_clipped_frames() {
        let balanced = RgbImage::from_pixel(100, 100, Rgb([128, 128, 128]));
        let black = RgbImage::from_pixel(100, 100, Rgb([0, 0, 0]));
        assert!(analyse(&balanced).exposure > analyse(&black).exposure);
    }

    #[test]
    fn similar_resizes_keep_the_same_hash() {
        let source = RgbImage::from_fn(180, 120, |x, _| {
            let value = ((x * 255) / 179) as u8;
            Rgb([value, value, value])
        });
        let resized = image::imageops::resize(&source, 900, 600, FilterType::Lanczos3);
        assert_eq!(analyse(&source).perceptual_hash, analyse(&resized).perceptual_hash);
    }
}
