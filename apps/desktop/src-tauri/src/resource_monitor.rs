//! Low-overhead Windows CPU and NVIDIA GPU sampling for the processing graph.

use std::path::PathBuf;
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[derive(Debug, Clone, Copy)]
pub struct ResourceUsage {
    pub cpu_percent: Option<f64>,
    pub gpu_percent: Option<f64>,
}

pub struct ResourceMonitor {
    cpu: SystemCpuSampler,
    nvidia_smi: Option<PathBuf>,
}

impl Default for ResourceMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceMonitor {
    pub fn new() -> Self {
        Self {
            cpu: SystemCpuSampler::new(),
            nvidia_smi: find_nvidia_smi(),
        }
    }

    pub fn sample(&mut self) -> ResourceUsage {
        ResourceUsage {
            cpu_percent: self.cpu.sample(),
            gpu_percent: self.nvidia_smi.as_deref().and_then(sample_nvidia_gpu),
        }
    }
}

fn find_nvidia_smi() -> Option<PathBuf> {
    let system = PathBuf::from(r"C:\Windows\System32\nvidia-smi.exe");
    if system.is_file() {
        return Some(system);
    }
    let nvsmi = PathBuf::from(r"C:\Program Files\NVIDIA Corporation\NVSMI\nvidia-smi.exe");
    if nvsmi.is_file() {
        return Some(nvsmi);
    }
    // Command's normal PATH lookup remains useful on development machines.
    Some(PathBuf::from("nvidia-smi.exe"))
}

fn sample_nvidia_gpu(executable: &std::path::Path) -> Option<f64> {
    let mut command = Command::new(executable);
    command.args(["--query-gpu=utilization.gpu", "--format=csv,noheader,nounits"]);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    // When more than one NVIDIA GPU is present, show the busiest device. The
    // graph is explicitly labelled as device-wide rather than process-only.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<f64>().ok())
        .reduce(f64::max)
        .map(|value| value.clamp(0.0, 100.0))
}

#[cfg(windows)]
struct SystemCpuSampler {
    last_ticks: Option<(u64, u64)>,
}

#[cfg(windows)]
impl SystemCpuSampler {
    fn new() -> Self {
        Self {
            last_ticks: system_ticks(),
        }
    }

    fn sample(&mut self) -> Option<f64> {
        let current = system_ticks()?;
        let previous = self.last_ticks.replace(current)?;
        let idle = current.0.saturating_sub(previous.0);
        let total = current.1.saturating_sub(previous.1);
        if total == 0 {
            return None;
        }
        let busy = total.saturating_sub(idle);
        Some((busy as f64 / total as f64 * 100.0).clamp(0.0, 100.0))
    }
}

#[cfg(windows)]
fn system_ticks() -> Option<(u64, u64)> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetSystemTimes;

    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: all three FILETIME output pointers live for the call. Kernel
    // time includes idle time, as documented by GetSystemTimes.
    let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
    if ok == 0 {
        return None;
    }
    Some((filetime_ticks(idle), filetime_ticks(kernel) + filetime_ticks(user)))
}

#[cfg(windows)]
fn filetime_ticks(value: windows_sys::Win32::Foundation::FILETIME) -> u64 {
    ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
}

#[cfg(not(windows))]
struct SystemCpuSampler;

#[cfg(not(windows))]
impl SystemCpuSampler {
    fn new() -> Self {
        Self
    }

    fn sample(&mut self) -> Option<f64> {
        None
    }
}
