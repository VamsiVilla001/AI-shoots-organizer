//! A thin wrapper over the `ffmpeg` and `ffprobe` executables.
//!
//! Linking FFmpeg as a library would drag a large native build into a project
//! that has to ship on both Windows and Apple Silicon; shelling out keeps the
//! dependency optional and the packaging simple. FFmpeg is used for three
//! things: probing containers, decoding the still formats the `image` crate
//! does not handle, and pulling sample frames out of video (§9).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use image::RgbImage;

use crate::{MediaError, Result};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

/// FFmpeg otherwise creates one decoder worker per logical CPU and can occupy
/// the whole machine on 4K intraframe footage. Two threads keep background
/// processing useful while leaving the UI and the editor's other applications
/// responsive.
const DECODE_THREADS: &str = "2";
const FILTER_THREADS: &str = "1";

/// Suppresses the console window that would otherwise flash up on Windows for
/// every single invocation.
fn command(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    }
    cmd
}

#[derive(Debug, Clone)]
pub struct Ffmpeg {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct VideoInfo {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration: Option<f64>,
    pub frame_rate: Option<f64>,
    pub rotation: i32,
    pub creation_time: Option<String>,
}

impl Ffmpeg {
    /// Looks for FFmpeg next to an explicitly configured directory first, then
    /// on `PATH`. Returns `None` when it is not installed — the application
    /// stays usable for JPEG/PNG shoots without it.
    pub fn discover(hint_dir: Option<&Path>) -> Option<Self> {
        let exe = |stem: &str| -> Option<PathBuf> {
            if let Some(dir) = hint_dir {
                let candidate = dir.join(if cfg!(windows) {
                    format!("{stem}.exe")
                } else {
                    stem.to_string()
                });
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
            which::which(stem).ok()
        };

        let ffmpeg = exe("ffmpeg")?;
        let ffprobe = exe("ffprobe")
            .unwrap_or_else(|| ffmpeg.with_file_name(if cfg!(windows) { "ffprobe.exe" } else { "ffprobe" }));

        Some(Self { ffmpeg, ffprobe })
    }

    pub fn ffmpeg_path(&self) -> &Path {
        &self.ffmpeg
    }

    /// The version banner, for the Settings screen.
    pub fn version(&self) -> Option<String> {
        let out = command(&self.ffmpeg).arg("-version").output().ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .map(|l| l.trim().to_string())
    }

    /// Container and stream facts, read without decoding any frames.
    pub fn probe(&self, path: &Path) -> Result<VideoInfo> {
        let out = command(&self.ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height,duration,r_frame_rate:stream_tags=rotate:\
                 stream_side_data=rotation:format=duration:format_tags=creation_time",
                "-of",
                "default=noprint_wrappers=1",
            ])
            .arg(path)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| MediaError::Ffmpeg(format!("ffprobe failed to start: {e}")))?;

        if !out.status.success() {
            return Err(MediaError::Ffmpeg(format!(
                "ffprobe exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }

        let mut info = VideoInfo::default();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            if value.is_empty() || value == "N/A" {
                continue;
            }
            match key.trim() {
                "width" => info.width = value.parse().ok(),
                "height" => info.height = value.parse().ok(),
                // The stream duration is absent for some containers; the format
                // duration is the fallback, so do not overwrite a good value.
                "duration" => info.duration = info.duration.or_else(|| value.parse().ok()),
                "r_frame_rate" => info.frame_rate = parse_rational(value),
                "rotation" | "TAG:rotate" => {
                    if let Ok(r) = value.parse::<f64>() {
                        info.rotation = r.round() as i32;
                    }
                }
                "TAG:creation_time" => info.creation_time = Some(value.to_string()),
                _ => {}
            }
        }
        Ok(info)
    }

    /// Decodes a still image FFmpeg understands but the `image` crate does not
    /// (HEIC, AVIF, camera raw), optionally downscaling on the way out.
    pub fn decode_still(&self, path: &Path, max_dim: Option<u32>) -> Result<RgbImage> {
        self.decode_image(path, None, max_dim)
    }

    /// Pulls a single frame at `timestamp`. Seeking before `-i` makes this a
    /// keyframe seek rather than a decode from the start of the file.
    pub fn extract_frame(&self, path: &Path, timestamp: f64, max_dim: Option<u32>) -> Result<RgbImage> {
        self.decode_image(path, Some(timestamp), max_dim)
    }

    fn decode_image(&self, path: &Path, timestamp: Option<f64>, max_dim: Option<u32>) -> Result<RgbImage> {
        let hardware = self.image_command(path, timestamp, max_dim, true);
        match self.run_to_image(hardware, path) {
            Ok(image) => Ok(image),
            Err(hardware_error) => {
                // Hardware decoding varies by codec, bit depth, driver and GPU.
                // Retry safely with the resource-capped software path instead
                // of making an otherwise supported file fail analysis.
                tracing::debug!(file = %path.display(), error = %hardware_error, "hardware video decode unavailable; using limited software decode");
                let software = self.image_command(path, timestamp, max_dim, false);
                self.run_to_image(software, path)
            }
        }
    }

    fn image_command(
        &self,
        path: &Path,
        timestamp: Option<f64>,
        max_dim: Option<u32>,
        hardware_acceleration: bool,
    ) -> Command {
        let mut cmd = self.constrained_command(hardware_acceleration);
        cmd.args(["-v", "error"]);
        if let Some(timestamp) = timestamp {
            cmd.args(["-ss", &format!("{timestamp:.3}")]);
        }
        cmd.arg("-i").arg(path);
        if let Some(max) = max_dim {
            // `force_original_aspect_ratio=decrease` never upscales a small source.
            cmd.args([
                "-vf",
                &format!("scale={max}:{max}:force_original_aspect_ratio=decrease"),
            ]);
        }
        // PPM is an uncompressed RGB stream. It removes the expensive PNG
        // encoder/decoder pair that used to run for every sampled frame while
        // retaining the exact pixels needed by face detection.
        cmd.args([
            "-an",
            "-sn",
            "-dn",
            "-frames:v",
            "1",
            "-pix_fmt",
            "rgb24",
            "-f",
            "image2pipe",
            "-vcodec",
            "ppm",
            "-",
        ]);
        cmd
    }

    fn constrained_command(&self, hardware_acceleration: bool) -> Command {
        let mut cmd = command(&self.ffmpeg);
        cmd.args([
            "-nostdin",
            "-hide_banner",
            "-threads",
            DECODE_THREADS,
            "-filter_threads",
            FILTER_THREADS,
        ]);
        if hardware_acceleration {
            cmd.args(["-hwaccel", "auto"]);
        }
        cmd
    }

    /// Timestamps where the picture changes substantially.
    ///
    /// The filter chain drops to `probe_fps` and scales down *before* the
    /// scene detector runs, so this costs a fraction of a full decode while
    /// still finding the cuts (§19).
    pub fn scene_changes(&self, path: &Path, threshold: f64, probe_fps: f64) -> Result<Vec<f64>> {
        let filter = format!("fps={probe_fps},scale=320:-2,select='gt(scene,{threshold})',showinfo",);
        let mut cmd = self.constrained_command(false);
        let out = cmd
            .args(["-v", "info", "-i"])
            .arg(path)
            .args(["-vf", &filter, "-an", "-sn", "-dn", "-f", "null", "-"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| MediaError::Ffmpeg(format!("ffmpeg failed to start: {e}")))?;

        // showinfo writes to stderr even on success.
        let stderr = String::from_utf8_lossy(&out.stderr);
        let mut times: Vec<f64> = stderr
            .lines()
            .filter_map(|line| {
                let idx = line.find("pts_time:")?;
                let rest = &line[idx + "pts_time:".len()..];
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                rest[..end].parse::<f64>().ok()
            })
            .collect();

        times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        times.dedup_by(|a, b| (*a - *b).abs() < 0.05);
        Ok(times)
    }

    fn run_to_image(&self, mut cmd: Command, path: &Path) -> Result<RgbImage> {
        let out = cmd
            .stdin(Stdio::null())
            .output()
            .map_err(|e| MediaError::Ffmpeg(format!("ffmpeg failed to start: {e}")))?;

        if !out.status.success() || out.stdout.is_empty() {
            return Err(MediaError::Ffmpeg(format!(
                "ffmpeg could not decode {}: {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }

        let img = image::load_from_memory_with_format(&out.stdout, image::ImageFormat::Pnm)
            .map_err(|e| MediaError::Decode(format!("{}: {e}", path.display())))?;
        Ok(img.to_rgb8())
    }
}

/// Parses ffprobe's `30000/1001` style frame rates.
fn parse_rational(value: &str) -> Option<f64> {
    match value.split_once('/') {
        Some((num, den)) => {
            let (num, den): (f64, f64) = (num.parse().ok()?, den.parse().ok()?);
            if den == 0.0 {
                None
            } else {
                Some(num / den)
            }
        }
        None => value.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frame_rate_rationals() {
        assert_eq!(parse_rational("30000/1001").map(|f| (f * 100.0).round()), Some(2997.0));
        assert_eq!(parse_rational("25/1"), Some(25.0));
        assert_eq!(parse_rational("0/0"), None);
        assert_eq!(parse_rational("59.94"), Some(59.94));
    }

    #[test]
    fn frame_decode_is_thread_limited_and_uses_lossless_lightweight_output() {
        let ffmpeg = Ffmpeg {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
        };
        let command = ffmpeg.image_command(Path::new("clip.mp4"), Some(12.5), Some(1280), true);
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert!(args.windows(2).any(|pair| pair == ["-threads", DECODE_THREADS]));
        assert!(args.windows(2).any(|pair| pair == ["-filter_threads", FILTER_THREADS]));
        assert!(args.windows(2).any(|pair| pair == ["-hwaccel", "auto"]));
        assert!(args.windows(2).any(|pair| pair == ["-vcodec", "ppm"]));
        assert!(!args.iter().any(|arg| arg == "png"));
    }
}
