use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=native/opencv_tracking.cpp");
    println!("cargo:rerun-if-env-changed=TEO_OPENCV_DIR");

    if env::var_os("CARGO_FEATURE_OPENCV_TRACKING").is_none() {
        return;
    }

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        panic!("opencv-tracking currently supports Windows builds only");
    }

    let build_root = find_build_root().unwrap_or_else(|| {
        panic!("OpenCV SDK not found. Run scripts/setup-opencv.ps1 or set TEO_OPENCV_DIR to the OpenCV build directory")
    });
    let include = build_root.join("include");
    let platform_root = ["vc17", "vc16"]
        .into_iter()
        .map(|toolset| build_root.join("x64").join(toolset))
        .find(|path| path.join("lib").is_dir())
        .unwrap_or_else(|| {
            panic!(
                "no supported x64 MSVC OpenCV library directory under {}",
                build_root.display()
            )
        });
    let library = find_world_library(&platform_root.join("lib")).unwrap_or_else(|| {
        panic!(
            "opencv_world release library not found under {}",
            platform_root.display()
        )
    });
    let link_name = library
        .file_stem()
        .and_then(|name| name.to_str())
        .expect("OpenCV library name should be UTF-8");

    cc::Build::new()
        .cpp(true)
        .include(&include)
        .file("native/opencv_tracking.cpp")
        .flag_if_supported("/std:c++17")
        .warnings(true)
        .compile("teo_opencv_tracking_bridge");

    println!("cargo:rustc-link-search=native={}", platform_root.join("lib").display());
    println!("cargo:rustc-link-lib=dylib={link_name}");

    let dll = platform_root.join("bin").join(format!("{link_name}.dll"));
    if !dll.is_file() {
        panic!("OpenCV runtime DLL not found at {}", dll.display());
    }
    copy_runtime_dll(&dll);
}

fn find_build_root() -> Option<PathBuf> {
    if let Some(root) = env::var_os("TEO_OPENCV_DIR").map(PathBuf::from) {
        if root.join("include/opencv2/core.hpp").is_file() {
            return Some(root);
        }
    }

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR")?);
    let repository = manifest.parent()?.parent()?;
    let local = repository.join(".opencv/sdk/opencv/build");
    local.join("include/opencv2/core.hpp").is_file().then_some(local)
}

fn find_world_library(directory: &Path) -> Option<PathBuf> {
    let mut libraries: Vec<PathBuf> = fs::read_dir(directory)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            let stem = path.file_stem().and_then(|name| name.to_str()).unwrap_or_default();
            path.extension().and_then(|extension| extension.to_str()) == Some("lib")
                && stem.starts_with("opencv_world")
                && !stem.ends_with('d')
        })
        .collect();
    libraries.sort();
    libraries.pop()
}

fn copy_runtime_dll(dll: &Path) {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let Some(profile_dir) = out_dir.ancestors().nth(3) else {
        panic!("unexpected Cargo OUT_DIR: {}", out_dir.display());
    };
    let file_name = dll.file_name().expect("OpenCV DLL has a filename");
    for destination in [profile_dir.to_path_buf(), profile_dir.join("deps")] {
        fs::create_dir_all(&destination).expect("create Cargo output directory");
        fs::copy(dll, destination.join(file_name)).expect("copy OpenCV runtime beside executable");
    }
}
