# Camera RAW support

## Architecture

The scanner has one central, case-insensitive RAW extension registry in
`crates/media-core/src/formats.rs`. It routes media as follows:

```text
JPEG / PNG / WebP / TIFF / BMP -> Rust image decoder
Camera RAW                     -> LibRaw
HEIC / HEIF / AVIF             -> FFmpeg still decoder
Video                           -> FFmpeg video pipeline
```

Extension matching only selects a candidate decoder. LibRaw opens and
identifies the container before pixels or metadata are accepted, so renaming a
non-RAW file to `.RAF` does not bypass format validation.

All photo consumers use the same normalised decode result: RGB pixels,
`source_format`, `decode_method`, and per-stage timings. Thumbnail generation,
full-image viewing, face detection, face embedding, quality scoring and export
therefore see RAW photos in the same way as ordinary photos.

## Decode policy

1. Create a fresh LibRaw context for the current worker and open the source
   path. Windows uses LibRaw's wide-path API, including Unicode and UNC paths.
2. Ask LibRaw for the camera's selected embedded preview.
3. Decode the preview in memory. Use it when its long edge is at least 640 px.
4. If the preview is absent, corrupt or too small, release that context and run
   a half-size LibRaw demosaic using fast bilinear processing, camera white
   balance, 8-bit sRGB output and bounded in-memory buffers.
5. Resize to the caller's requested analysis/thumbnail limit, apply EXIF
   orientation, and release all large intermediate buffers.

No temporary TIFF/JPEG sidecars are created and source files remain read-only.
The camera preview is normally both faster and closer to the image shown on the
camera; half-size demosaic is the correctness fallback.

## Formats

The current registry includes:

- Fujifilm RAF;
- Sony ARW and SRF-style generic RAW;
- Nikon NEF and NRW;
- Canon CR2 and CR3;
- Olympus ORF;
- Panasonic RW2 and Leica RWL;
- Adobe DNG;
- Pentax PEF;
- Samsung SRW;
- Hasselblad 3FR;
- Phase One IIQ;
- Epson ERF, Minolta MRW, Leaf MOS, Sigma X3F, Kodak KDC/DCR and Mamiya MEF.

Actual camera/model coverage follows the linked LibRaw version. Unsupported or
new containers fail with a stable RAW error instead of being sent to FFmpeg.

## Dependency and installation

The Rust dependency is `rawlib 0.7.1`, wrapping LibRaw 0.22.2. Windows MSVC
builds link the bundled static LibRaw library, so end users do not install a
DLL. Linux and macOS builds use a system LibRaw when available; release builders
should install `libraw` and make it visible to `pkg-config` before packaging.
FFmpeg remains required for video and HEIC/HEIF/AVIF, but not for camera RAW.

## Concurrency, memory and cache

RAW work uses the desktop application's existing persistent bounded queue. The
current desktop cap is two concurrent workers because each worker also owns
ONNX detector/embedder sessions; this is deliberately more conservative than a
standalone 4-8 image-decoder pool. Each RAW task owns a separate LibRaw context,
and no context is shared between threads.

Rendered thumbnails retain the existing content key (`path + size + modified
time`). A rescan of an unchanged RAW therefore reuses the cached JPEG thumbnail
without reopening or demosaicing the source.

## Errors and observability

RAW failures use stable codes suitable for the job log and UI:

- `RAW_UNSUPPORTED`
- `RAW_CORRUPT`
- `RAW_PREVIEW_UNAVAILABLE`
- `RAW_DECODE_FAILED`
- `RAW_OUT_OF_MEMORY`

Successful RAW decode logs contain filename/path, source format, LibRaw decode
method, output dimensions, result and timings for open, preview, full fallback,
resize and total decode. Photo analysis adds AI time, detected-face count and
end-to-end total time.

## Tests

Automated tests cover pipeline routing for RAF, ARW, NEF, CR2, CR3, DNG, ORF,
RW2, PEF, SRW, 3FR, IIQ, RWL and RAW; native JPEG/PNG routing; and MP4/MOV video
routing. Bitmap conversion and stable error codes are unit tested.

A real-file test is opt-in because camera originals are not committed:

```powershell
$env:TEO_RAW_FILE='\\server\share\shoot\DSCF1092.RAF'
cargo test -p teo-media-core --test real_raw_files -- --ignored --nocapture
```

The release acceptance matrix should include at least one genuine RAF, ARW,
NEF, CR2 or CR3, and DNG plus JPEG, PNG, MP4 and MOV. The RAF test asserts that
no FFmpeg instance is supplied, so routing regressions are caught directly.

## Known limitations

- Embedded preview size and colour rendering are camera-controlled. Very small
  previews trigger the demosaic fallback.
- LibRaw metadata does not expose every maker-specific field. Orientation is
  read defensively from the RAW container and then from the embedded JPEG.
- Newly released camera models may require a newer LibRaw release.
- Full half-size demosaic can still be expensive for unusually large sensor
  files; the worker cap prevents several such allocations from overwhelming
  the desktop process.
- Windows MSVC is the currently validated release target. macOS/Linux packaging
  must validate the system LibRaw linkage in CI before release.
