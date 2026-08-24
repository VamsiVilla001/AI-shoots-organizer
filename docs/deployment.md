# Deployment

How to get this app onto other people's machines — a Mac Studio, an edit-bay
PC, a colleague's laptop.

Two installers come out of a release:

| Platform | Artefact | Built where |
| --- | --- | --- |
| macOS 11+, Apple Silicon (Mac Studio, M-series) | `Esports AI Media Organiser_<version>_aarch64.dmg` | a Mac, or a `macos-14` CI runner |
| Windows 10/11 x64 | `Esports AI Media Organiser_<version>_x64-setup.exe` | Windows, or a `windows-latest` CI runner |

**A macOS app cannot be built on Windows.** It needs Apple's toolchain and
`codesign`. Use CI (path A) or the Mac itself (path B) — there is no
cross-compile shortcut.

## What ships inside, and what does not

- **The frontend, the shell binary, and `teo-server`** — always. The desktop app
  is a client of a private server it starts on loopback, so the installer ships
  both binaries; without the sidecar the app opens, says the local server did not
  start, and can do nothing else. `npm run package:win` builds and stages it.
- **The face models** (~280 MB) — only when the build machine has fetched them
  and the build passes `--config src-tauri/tauri.models.conf.json`. They are
  gitignored, so this is opt-in: a glob matching nothing is a hard build error,
  which is why it lives in a separate config file rather than the default one.
  With them bundled, the first launch installs them into the app data folder
  (`models::seed_from_bundle`) and face recognition works out of the box.
  Without them, the app still runs — sorting into groups and exporting need no
  models at all — and Settings explains what to fetch.
- **FFmpeg** — never bundled (size, and its licence terms). Needed for videos,
  HEIC and camera raw; JPEG/PNG shoots work without it. `brew install ffmpeg`
  on macOS, `winget install Gyan.FFmpeg` on Windows, or point
  **Settings → FFmpeg directory** at a copy.
- **GPU acceleration** — the providers are compiled in by target (CoreML on
  macOS, DirectML on Windows), and ONNX Runtime falls back to CPU whenever one
  cannot start, so nothing here can break an install. On macOS CoreML is part
  of the OS and needs nothing. On **Windows** it does need one file: Windows
  ships `DirectML.dll` 1.0 in System32, while ONNX Runtime wants the 1.15
  redistributable it downloads at build time. `npm run package:win` stages that
  DLL out of the download cache and ships it beside the executable; a build
  without it runs on CPU — the README's measurements put that at roughly 2.6x
  slower detection and 7.9x slower embedding. **Settings → Acceleration** shows
  what actually started, which is the quickest way to check a fresh install.

## Path A — CI builds both installers (recommended)

`.github/workflows/release.yml` builds macOS (Apple Silicon) and Windows on
every `v*` tag and attaches both installers to a **draft** GitHub release.

```bash
# bump the version in apps/desktop/src-tauri/tauri.conf.json, package.json
# and Cargo.toml (workspace.package.version) so they agree, then:
git tag v0.1.0
git push origin v0.1.0
```

Then open the draft release, check the two artefacts, and publish. Running the
workflow by hand from the Actions tab instead uploads the installers as run
artefacts without creating a release — useful for a test build.

The Mac job runs on `macos-14`, which is Apple Silicon: the same architecture as
a Mac Studio, so the `.dmg` it produces is a native arm64 build.

## Path B — build on the Mac Studio itself

Prerequisites: Xcode Command Line Tools (`xcode-select --install`), Rust
(`rustup`), Node 20+, and FFmpeg if videos are in scope.

```bash
git clone https://github.com/VamsiVilla001/AI-shoots-organizer.git
cd AI-shoots-organizer
bash scripts/build-macos.sh
```

The script checks the prerequisites, fetches the models, bundles them if they
are present, builds and stages the `teo-server` sidecar, builds for
`aarch64-apple-darwin`, and prints where the `.dmg` landed
(`target/aarch64-apple-darwin/release/bundle/dmg/`). Expect 10–20 minutes on a
cold build — ONNX Runtime and the webview crates dominate.

The sidecar step matters as much as the models: since the shell became a client
of `teo-server`, a `.dmg` built without it installs an app that opens, reports
that the local server did not start, and can do nothing else.

## Path C — Windows installer by hand

```bash
powershell -ExecutionPolicy Bypass -File scripts/fetch-models.ps1
npm run package:win
```

`package:win` builds `teo-server`, stages it and `DirectML.dll`, then builds with
the config overlays. **Close the app first** — a running copy holds both
binaries, and the staging step cannot overwrite a file that is in use.
The installer lands in `target/release/bundle/nsis/` and is ~192 MB: the binary,
the DirectML redistributable and the two models. Verified contents of a build
made this way:

```
teo-desktop.exe          6.0 MB   the shell: a window and a supervisor
teo-server.exe          27.5 MB   the application itself
DirectML.dll            18.5 MB
models/det_10g.onnx     16.9 MB
models/w600k_r50.onnx  174.4 MB
```

The shell shrank from 32 MB to 6 MB when it stopped linking the database and
ONNX Runtime; that work moved into the server, not away.

`npm run tauri:build -w @teo/desktop` on its own produces a ~32 MB installer
with none of those extras — fine for someone who will fetch models themselves.

The DLL is staged into `dist-resources/` rather than referenced inside
`target/`: the bundler cannot read a file cargo is still writing, which fails
the build with "used by another process".

The overlay is deliberately *not* called `tauri.windows.conf.json`. Tauri
auto-merges `tauri.<platform>.conf.json` on that platform, so that name would
pull the resource into every development build as well — including `npm run
dev`, where copying over the symlinked DLL that a running app has loaded fails
the build outright.

The NSIS installer is configured `perMachine`, so it asks for administrator
rights and installs for everyone on the machine. Switch
`bundle.windows.nsis.installMode` to `currentUser` if handing it to people who
cannot elevate.

> `cargo build --release` is **not** a production build. Tauri decides
> dev-versus-production from the `custom-protocol` cargo feature, which only
> the Tauri CLI sets — a plain cargo release build opens a window pointing at
> `localhost:1420` and shows "can't reach this page". Always build through
> `npm run tauri:build`.

## Signing

Unsigned builds work, but the first launch is hostile to the person receiving
them. This is the difference between "double-click it" and "no, really, it's
fine, right-click instead".

### macOS

Without a signature, Gatekeeper blocks the app. The recipient needs one of:

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

The workflow already passes all six through, so adding them as repository
secrets is enough — no workflow edit needed.

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
