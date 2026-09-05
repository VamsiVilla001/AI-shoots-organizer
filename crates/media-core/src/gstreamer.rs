//! GStreamer-backed full-video proxy generation.
//!
//! The runtime is invoked as a separate below-normal-priority process. This
//! keeps optional native multimedia dependencies out of the application
//! binary while still using GStreamer's decoder selection and codec plugins.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use crate::proxies::VIDEO_PROXY_WIDTH;
use crate::{MediaError, Result};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

const SOFTWARE_DECODE_THREADS: &str = "4";
const ENCODE_THREADS: &str = "2";
const VIDEO_BITRATE_KBPS: &str = "1200";
const AUDIO_BITRATE_BPS: &str = "96000";

/// A full proxy is expensive for 4K camera originals. Never let multiple
/// import workers run proxy transcodes at the same time.
static PROXY_GENERATION_GATE: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone)]
pub struct Gstreamer {
    launcher: PathBuf,
    discoverer: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
struct DiscoveredVideo {
    width: u32,
    height: u32,
    fps: f64,
    codec: String,
    has_audio: bool,
}

impl Gstreamer {
    pub fn discover() -> Option<Self> {
        let launcher = discover_executable("gst-launch-1.0")?;
        let discoverer_name = if cfg!(windows) {
            "gst-discoverer-1.0.exe"
        } else {
            "gst-discoverer-1.0"
        };
        let discoverer = launcher.with_file_name(discoverer_name);
        discoverer.is_file().then_some(Self { launcher, discoverer })
    }

    pub fn version(&self) -> Option<String> {
        let output = self.command(&self.launcher).arg("--version").output().ok()?;
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find(|line| line.contains("GStreamer"))
            .map(|line| line.trim().to_string())
    }

    /// Creates a complete 512px-wide H.264/AAC proxy. Frame rate is preserved
    /// because the pipeline intentionally contains no `videorate` element or
    /// frame-rate cap. The one-second GOP and front-loaded MP4 index make
    /// timeline seeking responsive in WebView2.
    pub fn create_video_proxy(&self, source: &Path, target: &Path, orientation: u16) -> Result<()> {
        if target.is_file() {
            return Ok(());
        }
        let _guard = PROXY_GENERATION_GATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if target.is_file() {
            return Ok(());
        }

        let info = self.inspect(source)?;
        let (source_width, source_height) = if matches!(orientation, 5..=8) {
            (info.height, info.width)
        } else {
            (info.width, info.height)
        };
        let output_height = scaled_even_height(source_width, source_height);
        let keyframe_interval = info.fps.round().clamp(1.0, 240.0) as u32;

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| MediaError::Io(format!("create {}: {error}", parent.display())))?;
        }
        let temporary = target.with_extension("mp4.part");
        let _ = std::fs::remove_file(&temporary);
        let mut command = self.proxy_command(
            source,
            &temporary,
            output_height,
            keyframe_interval,
            info.has_audio,
            software_decoder(&info.codec),
        );
        let output = command
            .stdin(Stdio::null())
            .output()
            .map_err(|error| MediaError::Gstreamer(format!("proxy process failed to start: {error}")))?;

        if !output.status.success() || !temporary.metadata().map(|metadata| metadata.len() > 0).unwrap_or(false) {
            let _ = std::fs::remove_file(&temporary);
            return Err(MediaError::Gstreamer(format!(
                "could not create proxy for {}: {}",
                source.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        std::fs::rename(&temporary, target)
            .map_err(|error| MediaError::Io(format!("finalise {}: {error}", target.display())))?;
        Ok(())
    }

    fn inspect(&self, source: &Path) -> Result<DiscoveredVideo> {
        let output = self
            .command(&self.discoverer)
            .args(["--timeout=30"])
            .arg(gstreamer_path(source))
            .stdin(Stdio::null())
            .output()
            .map_err(|error| MediaError::Gstreamer(format!("discovery failed to start: {error}")))?;
        if !output.status.success() {
            return Err(MediaError::Gstreamer(format!(
                "could not inspect {}: {}",
                source.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        parse_discovery(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
            MediaError::Gstreamer(format!("did not report usable video metadata for {}", source.display()))
        })
    }

    fn proxy_command(
        &self,
        source: &Path,
        target: &Path,
        output_height: u32,
        keyframe_interval: u32,
        has_audio: bool,
        decoder: Option<(&'static str, &'static str)>,
    ) -> Command {
        let mut command = self.command(&self.launcher);
        command.args(["-e", "-q", "filesrc"]);
        command.arg(format!("location={}", gstreamer_path(source)));

        if let Some((encoded_caps, decoder)) = decoder {
            command.args([
                "!",
                "parsebin",
                "name=dec",
                "dec.",
                "!",
                "queue",
                "max-size-buffers=4",
                "!",
            ]);
            command.arg(encoded_caps);
            command.args(["!", decoder]);
            command.arg(format!("max-threads={SOFTWARE_DECODE_THREADS}"));
        } else {
            command.args([
                "!",
                "decodebin",
                "name=dec",
                "dec.",
                "!",
                "queue",
                "max-size-buffers=4",
                "!",
            ]);
            command.arg("video/x-raw");
        }

        command.args(["!", "autovideoflip", "!", "videoconvert", "!", "videoscale", "!"]);
        command.arg(format!(
            "video/x-raw,width={VIDEO_PROXY_WIDTH},height={output_height},format=I420,pixel-aspect-ratio=1/1"
        ));
        command.args([
            "!",
            "x264enc",
            &format!("bitrate={VIDEO_BITRATE_KBPS}"),
            "speed-preset=ultrafast",
            &format!("key-int-max={keyframe_interval}"),
            "bframes=0",
            &format!("threads={ENCODE_THREADS}"),
            "!",
            "h264parse",
            "!",
            "queue",
            "!",
            "qtmux",
            "faststart=true",
            "name=mux",
            "!",
            "filesink",
        ]);
        command.arg(format!("location={}", gstreamer_path(target)));

        if has_audio {
            command.args([
                "dec.",
                "!",
                "queue",
                "max-size-buffers=16",
                "!",
                "decodebin",
                "!",
                "audio/x-raw",
                "!",
                "audioconvert",
                "!",
                "audioresample",
                "!",
                "avenc_aac",
                &format!("bitrate={AUDIO_BITRATE_BPS}"),
                "!",
                "aacparse",
                "!",
                "queue",
                "!",
                "mux.",
            ]);
        }
        command
    }

    fn command(&self, executable: &Path) -> Command {
        let mut command = Command::new(executable);
        // gst-discoverer's labels are parsed below, so force stable output
        // independently of the user's Windows/macOS display language.
        command.env("LANG", "C").env("LC_ALL", "C");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
        }
        command
    }
}

fn discover_executable(stem: &str) -> Option<PathBuf> {
    if let Ok(path) = which::which(stem) {
        return Some(path);
    }
    #[cfg(windows)]
    {
        let executable = format!("{stem}.exe");
        let mut roots = Vec::new();
        for variable in ["GSTREAMER_1_0_ROOT_MSVC_X86_64", "GSTREAMER_1_0_ROOT_X86_64"] {
            if let Some(root) = std::env::var_os(variable) {
                roots.push(PathBuf::from(root).join("bin"));
            }
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            roots.push(PathBuf::from(local).join("Programs/gstreamer/1.0/msvc_x86_64/bin"));
        }
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            roots.push(PathBuf::from(program_files).join("gstreamer/1.0/msvc_x86_64/bin"));
        }
        for root in roots {
            let candidate = root.join(&executable);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let system = PathBuf::from("/Library/Frameworks/GStreamer.framework/Versions/Current/bin").join(stem);
        if system.is_file() {
            return Some(system);
        }
        if let Some(home) = std::env::var_os("HOME") {
            let user = PathBuf::from(home)
                .join("Library/Frameworks/GStreamer.framework/Versions/Current/bin")
                .join(stem);
            if user.is_file() {
                return Some(user);
            }
        }
    }
    None
}

fn gstreamer_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn scaled_even_height(width: u32, height: u32) -> u32 {
    let scaled =
        ((u64::from(height) * u64::from(VIDEO_PROXY_WIDTH) + u64::from(width) / 2) / u64::from(width.max(1))) as u32;
    (scaled.max(2) + 1) & !1
}

fn software_decoder(codec: &str) -> Option<(&'static str, &'static str)> {
    let codec = codec.to_ascii_lowercase();
    if codec.contains("h.265") || codec.contains("hevc") {
        Some(("video/x-h265", "avdec_h265"))
    } else if codec.contains("h.264") || codec.contains("avc") {
        Some(("video/x-h264", "avdec_h264"))
    } else if codec.contains("vp9") {
        Some(("video/x-vp9", "avdec_vp9"))
    } else if codec.contains("vp8") {
        Some(("video/x-vp8", "avdec_vp8"))
    } else {
        None
    }
}

fn parse_discovery(output: &str) -> Option<DiscoveredVideo> {
    let mut width = None;
    let mut height = None;
    let mut fps = None;
    let mut codec = None;
    let mut has_audio = false;
    for line in output.lines().map(str::trim) {
        if line.starts_with("video #") {
            codec = line.split_once(':').map(|(_, value)| value.trim().to_string());
        } else if line.starts_with("audio #") {
            has_audio = true;
        } else if let Some(value) = line.strip_prefix("Width:") {
            width = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("Height:") {
            height = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("Frame rate:") {
            fps = parse_fraction(value.trim());
        }
    }
    Some(DiscoveredVideo {
        width: width?,
        height: height?,
        fps: fps?,
        codec: codec?,
        has_audio,
    })
}

fn parse_fraction(value: &str) -> Option<f64> {
    let (numerator, denominator) = value.split_once('/')?;
    let numerator = numerator.parse::<f64>().ok()?;
    let denominator = denominator.parse::<f64>().ok()?;
    (denominator > 0.0).then_some(numerator / denominator)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISCOVERY: &str = r#"
      video #1: H.265
        Width: 3840
        Height: 2160
        Frame rate: 60000/1001
      audio #2: Raw 16-bit PCM audio
    "#;

    #[test]
    fn parses_video_and_audio_discovery() {
        let info = parse_discovery(DISCOVERY).unwrap();
        assert_eq!(info.width, 3840);
        assert_eq!(info.height, 2160);
        assert!((info.fps - 59.940_059).abs() < 0.001);
        assert_eq!(info.codec, "H.265");
        assert!(info.has_audio);
    }

    #[test]
    fn computes_an_even_height_at_exactly_512_pixels_wide() {
        assert_eq!(scaled_even_height(3840, 2160), 288);
        assert_eq!(scaled_even_height(1920, 1080), 288);
        assert_eq!(scaled_even_height(1080, 1920), 910);
    }

    #[test]
    fn maps_common_codecs_to_thread_capped_decoders() {
        assert_eq!(software_decoder("H.265"), Some(("video/x-h265", "avdec_h265")));
        assert_eq!(software_decoder("H.264"), Some(("video/x-h264", "avdec_h264")));
        assert_eq!(software_decoder("AV1"), None);
    }

    #[test]
    fn proxy_pipeline_keeps_fps_adds_audio_and_is_seekable() {
        let runtime = Gstreamer {
            launcher: "gst-launch-1.0".into(),
            discoverer: "gst-discoverer-1.0".into(),
        };
        let command = runtime.proxy_command(
            Path::new("camera-original.mp4"),
            Path::new("proxy.mp4.part"),
            288,
            60,
            true,
            Some(("video/x-h265", "avdec_h265")),
        );
        let arguments: Vec<String> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();

        assert!(arguments
            .iter()
            .any(|argument| { argument == "video/x-raw,width=512,height=288,format=I420,pixel-aspect-ratio=1/1" }));
        assert!(!arguments.iter().any(|argument| argument == "videorate"));
        assert!(!arguments.iter().any(|argument| argument.contains("framerate=")));
        assert!(arguments.iter().any(|argument| argument == "avenc_aac"));
        assert!(arguments.iter().any(|argument| argument == "faststart=true"));
        assert!(arguments.iter().any(|argument| argument == "key-int-max=60"));
        assert!(arguments.iter().any(|argument| argument == "max-threads=4"));
    }

    #[test]
    #[ignore = "set TEO_VIDEO_FILE to validate a real GStreamer installation"]
    fn real_video_creates_a_complete_proxy() {
        let source = std::env::var_os("TEO_VIDEO_FILE").expect("set TEO_VIDEO_FILE");
        let runtime = Gstreamer::discover().expect("GStreamer runtime not found");
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("proxy.mp4");
        runtime
            .create_video_proxy(Path::new(&source), &target, 1)
            .expect("proxy generation failed");
        assert!(target.metadata().unwrap().len() > 0);
        let source_info = runtime.inspect(Path::new(&source)).unwrap();
        let proxy_info = runtime.inspect(&target).unwrap();
        assert_eq!(proxy_info.width, VIDEO_PROXY_WIDTH);
        assert!((proxy_info.fps - source_info.fps).abs() < 0.001);
        assert_eq!(proxy_info.has_audio, source_info.has_audio);
    }
}
