//! A thin wrapper over the `ffmpeg` and `ffprobe` executables.
//!
//! Linking FFmpeg as a library would drag a large native build into a project
//! that has to ship on both Windows and Apple Silicon; shelling out keeps the
//! dependency optional and the packaging simple. FFmpeg is used for three
//! things: probing containers, decoding the still formats the `image` crate
//! does not handle, and pulling sample frames out of video (§9).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use image::RgbImage;

use crate::proxies::VIDEO_PROXY_WIDTH;
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
const PROXY_VIDEO_BITRATE: &str = "1200k";
const PROXY_AUDIO_BITRATE: &str = "96k";

/// Proxy generation is deliberately single-file. Analysis already uses the
/// available GPU in parallel; several simultaneous transcodes would contend
/// for decoder, encoder and disk bandwidth without improving interactivity.
static PROXY_GENERATION_GATE: Mutex<()> = Mutex::new(());
static NVENC_AVAILABLE: OnceLock<bool> = OnceLock::new();

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

/// The path FFmpeg used to create a proxy. This is logged so a machine with an
/// outdated NVIDIA driver never silently falls back to CPU conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyBackend {
    Cached,
    Cuda,
    Nvenc,
    Cpu,
}

#[derive(Debug, Clone, Copy)]
enum ProxyMode {
    Cuda,
    Nvenc,
    Cpu,
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

/// The executable file name for a tool on this platform.
fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

/// Where package managers put FFmpeg, searched when `PATH` does not have it.
///
/// A `PATH` miss does not mean FFmpeg is absent. Every Windows installer
/// updates the *persisted* environment, but a process inherits its `PATH` at
/// launch and keeps it for life — so an application started from a shell that
/// predates the install cannot see a perfectly working FFmpeg until it is
/// restarted. Looking where the installers actually put things turns that
/// confusing "not found" into working video analysis.
fn well_known_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let env_dir = |key: &str| std::env::var_os(key).map(PathBuf::from);

    if cfg!(windows) {
        if let Some(local) = env_dir("LOCALAPPDATA") {
            // winget shims, then the package itself: its build directory is
            // named after the version, so the level below has to be walked.
            dirs.push(local.join("Microsoft\\WinGet\\Links"));
            let packages = local.join("Microsoft\\WinGet\\Packages");
            if let Ok(entries) = std::fs::read_dir(&packages) {
                for package in entries
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().to_ascii_lowercase().contains("ffmpeg"))
                {
                    dirs.push(package.path().join("bin"));
                    if let Ok(builds) = std::fs::read_dir(package.path()) {
                        dirs.extend(builds.flatten().map(|build| build.path().join("bin")));
                    }
                }
            }
            dirs.push(local.join("Programs\\ffmpeg\\bin"));
        }
        if let Some(profile) = env_dir("USERPROFILE") {
            dirs.push(profile.join("scoop\\shims"));
        }
        let program_data = env_dir("ProgramData").unwrap_or_else(|| PathBuf::from("C:\\ProgramData"));
        dirs.push(program_data.join("chocolatey\\bin"));
        dirs.push(program_data.join("chocolatey\\lib\\ffmpeg\\tools\\ffmpeg\\bin"));
        if let Some(program_files) = env_dir("ProgramFiles") {
            dirs.push(program_files.join("ffmpeg\\bin"));
        }
        dirs.push(PathBuf::from("C:\\ffmpeg\\bin"));
    } else {
        // Homebrew (Apple Silicon, then Intel) and MacPorts.
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
        dirs.push(PathBuf::from("/opt/local/bin"));
    }

    dirs
}

impl Ffmpeg {
    /// Looks for FFmpeg in an explicitly configured directory first, then on
    /// `PATH`, then in the locations package managers install it. Returns
    /// `None` when it really is not installed — the application stays usable
    /// for JPEG/PNG shoots without it.
    pub fn discover(hint_dir: Option<&Path>) -> Option<Self> {
        let in_dir = |dir: &Path, stem: &str| -> Option<PathBuf> {
            let candidate = dir.join(exe_name(stem));
            candidate.is_file().then_some(candidate)
        };

        let exe = |stem: &str| -> Option<PathBuf> {
            if let Some(found) = hint_dir.and_then(|dir| in_dir(dir, stem)) {
                return Some(found);
            }
            if let Ok(found) = which::which(stem) {
                return Some(found);
            }
            well_known_dirs().iter().find_map(|dir| in_dir(dir, stem))
        };

        let ffmpeg = exe("ffmpeg")?;
        // ffprobe ships beside ffmpeg in every distribution, so prefer its
        // sibling over a differently-versioned one found elsewhere.
        let sibling = ffmpeg.with_file_name(exe_name("ffprobe"));
        let ffprobe = if sibling.is_file() {
            sibling
        } else {
            exe("ffprobe").unwrap_or(sibling)
        };

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

    /// Whether FFmpeg can start the NVIDIA H.264 encoder with the installed
    /// driver. Listing `h264_nvenc` alone is insufficient because the build
    /// may require a newer driver API than the machine provides.
    pub fn nvenc_available(&self) -> bool {
        *NVENC_AVAILABLE.get_or_init(|| self.probe_nvenc())
    }

    /// Creates a complete, browser-compatible 512px H.264/AAC proxy.
    ///
    /// Supported NVIDIA sources stay on the GPU for decode, resize and encode.
    /// Camera formats that NVDEC cannot decode (notably HEVC 10-bit 4:2:2) use
    /// the CPU decoder and NVIDIA encoder. If NVENC is unavailable, a tightly
    /// bounded two-thread x264 conversion keeps proxy creation functional.
    pub fn create_video_proxy(&self, source: &Path, target: &Path, orientation: u16) -> Result<ProxyBackend> {
        if target.is_file() {
            return Ok(ProxyBackend::Cached);
        }
        let _guard = PROXY_GENERATION_GATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if target.is_file() {
            return Ok(ProxyBackend::Cached);
        }

        let info = self.probe(source)?;
        let keyframe_interval = info.frame_rate.unwrap_or(30.0).round().clamp(1.0, 240.0) as u32;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| MediaError::Io(format!("create {}: {error}", parent.display())))?;
        }
        let temporary = target.with_extension("mp4.part");
        let _ = std::fs::remove_file(&temporary);

        let nvenc = self.nvenc_available();
        let mut failures = Vec::new();
        if nvenc && orientation == 1 {
            match self.run_proxy_attempt(source, &temporary, orientation, keyframe_interval, ProxyMode::Cuda) {
                Ok(()) => return self.finish_proxy(&temporary, target, ProxyBackend::Cuda),
                Err(error) => failures.push(format!("CUDA decode/scale: {error}")),
            }
        }
        if nvenc {
            match self.run_proxy_attempt(source, &temporary, orientation, keyframe_interval, ProxyMode::Nvenc) {
                Ok(()) => return self.finish_proxy(&temporary, target, ProxyBackend::Nvenc),
                Err(error) => failures.push(format!("NVENC: {error}")),
            }
        }

        match self.run_proxy_attempt(source, &temporary, orientation, keyframe_interval, ProxyMode::Cpu) {
            Ok(()) => {
                if !failures.is_empty() {
                    tracing::warn!(file = %source.display(), failures = %failures.join("; "),
                        "GPU proxy conversion unavailable; used limited CPU conversion");
                }
                self.finish_proxy(&temporary, target, ProxyBackend::Cpu)
            }
            Err(error) => {
                failures.push(format!("CPU: {error}"));
                let _ = std::fs::remove_file(&temporary);
                Err(MediaError::Ffmpeg(format!(
                    "could not create proxy for {}: {}",
                    source.display(),
                    failures.join("; ")
                )))
            }
        }
    }

    fn probe_nvenc(&self) -> bool {
        let output = command(&self.ffmpeg)
            .args([
                "-nostdin",
                "-hide_banner",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=size=256x144:rate=1",
                "-frames:v",
                "1",
                "-an",
                "-c:v",
                "h264_nvenc",
                "-f",
                "null",
                "-",
            ])
            .stdin(Stdio::null())
            .output();
        match output {
            Ok(output) if output.status.success() => true,
            Ok(output) => {
                tracing::warn!(error = %stderr_tail(&output.stderr),
                    "NVIDIA proxy encoding is unavailable; update the NVIDIA driver to enable it");
                false
            }
            Err(error) => {
                tracing::warn!(%error, "could not test NVIDIA proxy encoding");
                false
            }
        }
    }

    fn run_proxy_attempt(
        &self,
        source: &Path,
        temporary: &Path,
        orientation: u16,
        keyframe_interval: u32,
        mode: ProxyMode,
    ) -> std::result::Result<(), String> {
        let _ = std::fs::remove_file(temporary);
        let mut cmd = self.proxy_command(source, temporary, orientation, keyframe_interval, mode);
        let output = cmd
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("process failed to start: {error}"))?;
        let complete = temporary.metadata().map(|metadata| metadata.len() > 0).unwrap_or(false);
        if output.status.success() && complete {
            Ok(())
        } else {
            let _ = std::fs::remove_file(temporary);
            Err(stderr_tail(&output.stderr))
        }
    }

    fn finish_proxy(&self, temporary: &Path, target: &Path, backend: ProxyBackend) -> Result<ProxyBackend> {
        std::fs::rename(temporary, target)
            .map_err(|error| MediaError::Io(format!("finalise {}: {error}", target.display())))?;
        Ok(backend)
    }

    fn proxy_command(
        &self,
        source: &Path,
        temporary: &Path,
        orientation: u16,
        keyframe_interval: u32,
        mode: ProxyMode,
    ) -> Command {
        let mut cmd = command(&self.ffmpeg);
        cmd.args([
            "-nostdin",
            "-hide_banner",
            "-y",
            "-v",
            "error",
            "-threads",
            DECODE_THREADS,
            "-filter_threads",
            FILTER_THREADS,
            "-noautorotate",
        ]);
        if matches!(mode, ProxyMode::Cuda) {
            cmd.args(["-hwaccel", "cuda", "-hwaccel_output_format", "cuda"]);
        }
        cmd.arg("-i").arg(source);
        cmd.args(["-map", "0:v:0", "-map", "0:a:0?", "-map_metadata", "-1"]);

        let filter = match mode {
            ProxyMode::Cuda => format!("scale_cuda={VIDEO_PROXY_WIDTH}:-2:format=nv12"),
            ProxyMode::Nvenc | ProxyMode::Cpu => proxy_filter(orientation),
        };
        cmd.args(["-vf", &filter]);

        match mode {
            ProxyMode::Cuda | ProxyMode::Nvenc => {
                cmd.args(["-c:v", "h264_nvenc", "-preset", "p1", "-tune", "ll", "-rc", "vbr"]);
            }
            ProxyMode::Cpu => {
                cmd.args([
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-tune",
                    "fastdecode",
                    "-threads",
                    "2",
                ]);
            }
        }
        cmd.args([
            "-b:v",
            PROXY_VIDEO_BITRATE,
            "-maxrate",
            "1800k",
            "-bufsize",
            "2400k",
            "-g",
            &keyframe_interval.to_string(),
            "-keyint_min",
            &keyframe_interval.to_string(),
            "-bf",
            "0",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            PROXY_AUDIO_BITRATE,
            "-movflags",
            "+faststart",
            "-f",
            "mp4",
        ]);
        cmd.arg(temporary);
        cmd
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

fn proxy_filter(orientation: u16) -> String {
    let orientation_filter = match orientation {
        2 => "hflip,",
        3 => "hflip,vflip,",
        4 => "vflip,",
        5 => "hflip,transpose=clock,",
        6 => "transpose=clock,",
        7 => "hflip,transpose=cclock,",
        8 => "transpose=cclock,",
        _ => "",
    };
    format!("{orientation_filter}scale={VIDEO_PROXY_WIDTH}:-2:flags=fast_bilinear,format=yuv420p")
}

fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let mut tail: String = text
        .chars()
        .rev()
        .take(2000)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if tail.trim().is_empty() {
        tail = "FFmpeg exited without an error message".into();
    }
    tail.trim().replace(['\r', '\n'], " ")
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

    #[test]
    fn proxy_commands_prefer_cuda_and_keep_cpu_fallback_bounded() {
        let ffmpeg = Ffmpeg {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
        };
        let cuda = ffmpeg.proxy_command(Path::new("clip.mp4"), Path::new("proxy.part"), 1, 60, ProxyMode::Cuda);
        let cuda_args: Vec<_> = cuda.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert!(cuda_args.windows(2).any(|pair| pair == ["-hwaccel", "cuda"]));
        assert!(cuda_args.iter().any(|arg| arg.starts_with("scale_cuda=512:")));
        assert!(cuda_args.windows(2).any(|pair| pair == ["-c:v", "h264_nvenc"]));

        let cpu = ffmpeg.proxy_command(Path::new("clip.mp4"), Path::new("proxy.part"), 6, 60, ProxyMode::Cpu);
        let cpu_args: Vec<_> = cpu.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert!(cpu_args
            .iter()
            .any(|arg| arg == "transpose=clock,scale=512:-2:flags=fast_bilinear,format=yuv420p"));
        assert!(cpu_args.windows(2).any(|pair| pair == ["-c:v", "libx264"]));
        assert!(cpu_args.windows(2).any(|pair| pair == ["-threads", "2"]));
    }

    #[test]
    #[ignore = "requires an installed FFmpeg runtime"]
    fn installed_ffmpeg_creates_a_playable_proxy() {
        let ffmpeg = Ffmpeg::discover(None).expect("FFmpeg is not installed");
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.mp4");
        let target = directory.path().join("proxy.mp4");
        let generated = command(ffmpeg.ffmpeg_path())
            .args([
                "-nostdin",
                "-hide_banner",
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=1280x720:rate=30:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-c:v",
                "libx264",
                "-c:a",
                "aac",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(generated.success());

        let backend = ffmpeg.create_video_proxy(&source, &target, 1).unwrap();
        assert!(matches!(
            backend,
            ProxyBackend::Cuda | ProxyBackend::Nvenc | ProxyBackend::Cpu
        ));
        assert!(target.metadata().unwrap().len() > 0);
        let info = ffmpeg.probe(&target).unwrap();
        assert_eq!(info.width, Some(VIDEO_PROXY_WIDTH));
        assert_eq!(info.height, Some(288));
    }
}
