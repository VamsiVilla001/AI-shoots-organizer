# macOS build

Apple Silicon (`arm64`) build of Esports AI Media Organiser 0.1.0 from the
`macOS` branch.

- `Esports AI Media Organiser_0.1.0_aarch64.dmg` is the installer.
- `Esports AI Media Organiser_0.1.0_aarch64.app.zip` contains the standalone
  application bundle.
- Both packages include the `teo-server` sidecar and the two ONNX face models.
- The build is ad-hoc signed but not notarized. On first launch, right-click
  the app and choose **Open**.
- FFmpeg is not bundled and must be installed separately for video, HEIC, and
  camera RAW support.

The binary packages are stored with Git LFS. Verify downloads using
`SHA256SUMS`.
