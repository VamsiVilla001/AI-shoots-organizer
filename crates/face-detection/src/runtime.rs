//! ONNX Runtime session setup and execution-provider selection (§13).
//!
//! One model format, several backends: DirectML on Windows, CoreML on Apple
//! Silicon, CPU everywhere. Providers are registered in preference order and
//! ONNX Runtime falls back automatically when one cannot be initialised, so a
//! machine without a suitable GPU still works — just slower.

use std::path::Path;

use ort::session::{builder::GraphOptimizationLevel, Session};
use serde::{Deserialize, Serialize};

use crate::{FaceError, Result};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Accelerator {
    /// Pick the best provider this build and machine support.
    #[default]
    Auto,
    Cpu,
    DirectMl,
    CoreMl,
    Cuda,
}

impl Accelerator {
    pub fn as_str(&self) -> &'static str {
        match self {
            Accelerator::Auto => "auto",
            Accelerator::Cpu => "cpu",
            Accelerator::DirectMl => "directml",
            Accelerator::CoreMl => "coreml",
            Accelerator::Cuda => "cuda",
        }
    }
}

/// Which accelerators this binary was actually compiled with. The Settings
/// screen shows this so the choice on offer matches reality.
pub fn available_accelerators() -> Vec<Accelerator> {
    let mut out = vec![Accelerator::Auto, Accelerator::Cpu];
    if cfg!(all(feature = "directml", target_os = "windows")) {
        out.push(Accelerator::DirectMl);
    }
    if cfg!(all(feature = "coreml", target_os = "macos")) {
        out.push(Accelerator::CoreMl);
    }
    if cfg!(feature = "cuda") {
        out.push(Accelerator::Cuda);
    }
    out
}

#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub accelerator: Accelerator,
    /// Threads per session. Workers already run in parallel, so letting every
    /// session claim every core would oversubscribe the machine badly.
    pub intra_threads: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            accelerator: Accelerator::Auto,
            intra_threads: (num_cpus::get() / 2).max(1),
        }
    }
}

/// Builds a session for `model_path`, applying the requested provider.
pub fn build_session(model_path: &Path, config: &SessionConfig) -> Result<Session> {
    if !model_path.is_file() {
        return Err(FaceError::ModelMissing(model_path.display().to_string()));
    }

    let mut builder = Session::builder()
        .map_err(|e| FaceError::Runtime(e.to_string()))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| FaceError::Runtime(e.to_string()))?
        .with_intra_threads(config.intra_threads)
        .map_err(|e| FaceError::Runtime(e.to_string()))?;

    let providers = providers_for(config.accelerator);
    if !providers.is_empty() {
        // Registration is best-effort: if a GPU provider cannot start, ONNX
        // Runtime keeps the CPU provider that is always appended last.
        builder = match builder.with_execution_providers(providers) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "falling back to CPU execution");
                Session::builder()
                    .map_err(|e| FaceError::Runtime(e.to_string()))?
                    .with_optimization_level(GraphOptimizationLevel::Level3)
                    .map_err(|e| FaceError::Runtime(e.to_string()))?
                    .with_intra_threads(config.intra_threads)
                    .map_err(|e| FaceError::Runtime(e.to_string()))?
            }
        };
    }

    builder
        .commit_from_file(model_path)
        .map_err(|e| FaceError::Runtime(format!("loading {}: {e}", model_path.display())))
}

fn providers_for(accelerator: Accelerator) -> Vec<ort::ep::ExecutionProviderDispatch> {
    use ort::ep::CPU;

    let mut providers: Vec<ort::ep::ExecutionProviderDispatch> = Vec::new();

    let want_gpu = matches!(
        accelerator,
        Accelerator::Auto | Accelerator::DirectMl | Accelerator::CoreMl | Accelerator::Cuda
    );

    if want_gpu {
        #[cfg(all(feature = "cuda", not(target_os = "macos")))]
        if matches!(accelerator, Accelerator::Auto | Accelerator::Cuda) {
            providers.push(ort::ep::CUDA::default().build());
        }
        #[cfg(all(feature = "directml", target_os = "windows"))]
        if matches!(accelerator, Accelerator::Auto | Accelerator::DirectMl) {
            providers.push(ort::ep::DirectML::default().build());
        }
        #[cfg(all(feature = "coreml", target_os = "macos"))]
        if matches!(accelerator, Accelerator::Auto | Accelerator::CoreMl) {
            providers.push(ort::ep::CoreML::default().build());
        }
    }

    // Always last, always present.
    providers.push(CPU::default().build());
    providers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_is_always_the_final_fallback() {
        for accelerator in [Accelerator::Auto, Accelerator::Cpu, Accelerator::DirectMl, Accelerator::Cuda] {
            let providers = providers_for(accelerator);
            assert!(!providers.is_empty(), "{accelerator:?} produced no providers");
        }
    }

    #[test]
    fn available_list_always_offers_auto_and_cpu() {
        let available = available_accelerators();
        assert!(available.contains(&Accelerator::Auto));
        assert!(available.contains(&Accelerator::Cpu));
    }

    #[test]
    fn missing_model_is_reported_clearly() {
        let err = build_session(Path::new("no-such-model.onnx"), &SessionConfig::default()).unwrap_err();
        assert!(matches!(err, FaceError::ModelMissing(_)));
    }
}
