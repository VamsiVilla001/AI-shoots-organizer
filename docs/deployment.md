# Deployment

How to get this app onto other people's machines — a Mac Studio, an edit-bay
PC, a colleague's laptop.

Two installers come out of a release:

| Platform | Artefact | Built where |
| --- | --- | --- |
| macOS 11+, Apple Silicon (Mac Studio, M-series) | `Esports AI Media Organiser_<version>_aarch64.dmg` | a Mac, or a `macos-14` CI runner |
| Windows 10/11 x64 | `Esports AI Media Organiser_<version>_x64-setup.exe` | Windows, or a `windows-latest` CI runner |

**A macOS app cannot be built on Windows.** It needs Apple's toolchain and
`codesign`. Build on a Mac or a macOS CI runner; there is no cross-compile
shortcut.

## What ships inside, and what does not

- **The React frontend and native Tauri application** — always. V2 runs its
  SQLite job queue, media pipeline and AI engine in the application process;
  there is no server sidecar.
- **The face models** (~190 MB) — `npm run package:mac` fetches both ONNX files
  and bundles them. On first launch, the application copies missing bundled
  models into its app-data folder without overwriting any model the user has
  installed. A development build without models still runs: manual grouping
  and export do not require face recognition.
- **FFmpeg** — never bundled (size, and its licence terms). Needed for videos
  and HEIC; JPEG/PNG and camera RAW shoots work without it. `brew install ffmpeg`
  on macOS, `winget install Gyan.FFmpeg` on Windows, or point
  **Settings → FFmpeg directory** at a copy.
- **GPU acceleration** — CoreML is compiled into the macOS build and needs no
  separate installation. ONNX Runtime falls back to CPU whenever CoreML cannot
  start. **Settings → Acceleration** shows the providers available in the
  current build.

## Build on the Mac Studio itself

Prerequisites: Xcode Command Line Tools (`xcode-select --install`), Rust
(`rustup`), Node 20+, and FFmpeg if videos are in scope.

```bash
git clone https://github.com/VamsiVilla001/AI-shoots-organizer.git
cd AI-shoots-organizer
npm run package:mac
```

The script checks prerequisites, installs locked JavaScript dependencies,
fetches and bundles the models, builds for the host Mac architecture, signs the
complete bundle, verifies the signature and DMG, and prints both output paths.
On Apple Silicon the DMG lands under
`target/aarch64-apple-darwin/release/bundle/dmg/`. Expect 10–20 minutes on a
cold build — ONNX Runtime and the webview crates dominate.

> `cargo build --release` is **not** a production build. Tauri decides
> dev-versus-production from the `custom-protocol` cargo feature, which only
> the Tauri CLI sets — a plain cargo release build opens a window pointing at
> `localhost:1420` and shows "can't reach this page". Always build through
> `npm run tauri:build`.

## Signing

Local packages receive a complete ad-hoc signature. This lets macOS validate
the bundle structure, but does not establish a trusted developer identity.

### macOS

Without Developer ID signing and notarisation, the recipient needs one of:

- right-click the app in Applications → **Open** → **Open** again, or
- `xattr -dr com.apple.quarantine "/Applications/Esports AI Media Organiser.app"`

To sign and notarise properly you need the Apple Developer Program ($99/yr), a
**Developer ID Application** certificate, and an app-specific password. Tauri
reads these from the environment, locally or as CI secrets:

| Variable | What it is |
| --- | --- |
| `APPLE_CERTIFICATE` | the `.p12` certificate, base64-encoded |
| `APPLE_CERTIFICATE_PASSWORD` | its export password |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | the Apple ID email used for notarisation |
| `APPLE_PASSWORD` | an app-specific password for that Apple ID |
| `APPLE_TEAM_ID` | the 10-character team id |

Pass these variables to `npm run package:mac` locally or configure them as
secrets in the macOS CI job that runs the package command.

### Windows

Unsigned installers trigger SmartScreen ("unrecognised app"), which the user
can click past via **More info → Run anyway**. Removing that warning needs a
code-signing certificate (an OV certificate warms up reputation slowly; Azure
Trusted Signing is the cheaper modern route). Configure it under
`bundle.windows.signCommand` in `tauri.conf.json` when you have one.

## What the recipient should know

- **Nothing is uploaded.** Everything runs locally; the source folder is only
  ever read from.
- **Their data lives in** `~/Library/Application Support/com.teorganiser.desktop`
  (macOS) or `%APPDATA%\com.teorganiser.desktop` (Windows): the index, the
  thumbnails, the models. Deleting it loses the sorting, never the footage.
- **NAS shoots need the share mounted** before the folder picker can see it —
  Finder → Go → Connect to Server on macOS, a mapped drive or UNC path on
  Windows. The app needs read on the source and write on the destination.
- **First launch of a large shoot indexes in the background.** Sorting into
  groups works while it does.

## Updating an installed copy

There is no auto-updater configured: a new version means a new installer.
Reinstalling keeps the database, since it lives in the app data folder rather
than beside the app.

If updates get frequent enough to be annoying, Tauri's updater plugin can be
added — it needs a signing key pair, `createUpdaterArtifacts` turned on, and a
static JSON endpoint (GitHub Releases is fine). Worth it once other people
depend on the app; unnecessary while it is one or two editors.
