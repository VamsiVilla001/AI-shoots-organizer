# Deployment

How to get this app onto other people's machines — a Mac Studio, an edit-bay
PC, a colleague's laptop.

Two installers come out of a release:

| Platform | Artefact | Built where |
| --- | --- | --- |
| macOS 11+, Apple Silicon (Mac Studio, M-series) | `SKWAD Media Organiser_<version>_aarch64.dmg` | a Mac, or a `macos-14` CI runner |
| Windows 10/11 x64 | `SKWAD Media Organiser_<version>_x64-setup.exe` | Windows, or a `windows-latest` CI runner |

**A macOS app cannot be built on Windows.** It needs Apple's toolchain and
`codesign`. Use CI (path A) or the Mac itself (path B) — there is no
cross-compile shortcut.

## What ships inside, and what does not

- **PostgreSQL** — never. This is the one genuinely new prerequisite: the index
  moved out of an embedded `media.db` and into a real server, so a machine needs
  one reachable before the app can open a library at all. It is not bundled:
  the installers are per-user and unsigned-driver-free, while a database server
  wants a service account, a data directory and an upgrade path of its own —
  shipping one inside an app bundle is how you end up with an un-upgradable
  database nobody can back up.
  - **A single edit bay:** install PostgreSQL 17 on that machine and run
    `npm run db:setup`. The app talks to `localhost` and nothing changes for
    the editor.
  - **A studio:** one server everyone connects to, which is what makes a shared
    library work properly for the first time — see [one library, shared across
    the network](shared-library.md).
  - The server must have **ICU collation support** (every mainstream build
    does). The schema's `nocase` collation is built on it, and without it the
    first migration fails rather than silently sorting differently.
- **The frontend, the shell binary, and `skwad-server`** — always. The desktop app
  is a client of a private server it starts on loopback, so the installer ships
  both binaries; without the sidecar the app opens, says the local server did not
  start, and can do nothing else. `npm run package:win` builds and stages it.
- **The face models** (~191 MB) — **not currently, on any build path.**
  `tauri.conf.json` lists only the Premiere panel under `bundle.resources`, and
  there is no `tauri.models.conf.json` overlay and no `models::seed_from_bundle`
  to copy them out of a bundle on first launch. An earlier revision of this
  document described all three; none of them exist in the tree.
  In practice the models are fetched once per machine (`scripts/fetch-models.ps1`
  / `.sh`) and live in `<library folder>/models/`, which is where
  `ModelRegistry` looks. Without them the app still runs — indexing, sorting
  into groups and exporting need no models at all — and Settings explains what
  to fetch. Making a self-contained installer means adding the resource entry
  *and* the seeding code; the `with_models` input on the release workflow
  fetches them into the build, which is only half of it.
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
- **The Premiere Pro panel** — always, as a `.ccx` inside the bundle.
  `npm run package:premiere-panel` builds it and `npm run build` runs that
  first, so a release cannot accidentally ship without it. See below.

## The catalogue backend as a Windows service

`skwad-backend` signs and rewraps `.skwad` packages and holds the organisation
signing key. It is not on the app's critical path — the desktop app only calls
it when someone publishes or loads a catalogue — but it has to be up at that
moment, and it is the one component that genuinely wants to outlive a login
session. It registers itself:

```powershell
# In an elevated terminal. Copy the release binary somewhere stable first —
# a service running out of target\ breaks on the next `cargo clean`.
mkdir "C:\Program Files\SKWAD"
copy target\release\skwad-backend.exe "C:\Program Files\SKWAD\"
& "C:\Program Files\SKWAD\skwad-backend.exe" install
& "C:\Program Files\SKWAD\skwad-backend.exe" start
```

`install` does four things, together, because doing them separately is how the
signing key ends up world-readable:

1. Generates a signing keypair, a wrapping keypair and an auth token — **unless
   `C:\ProgramData\SKWAD\backend.env` already exists**, which it never
   overwrites. Replacing that file invalidates every catalogue already
   published under the old key, so rotating is a deliberate act: delete the
   file yourself first.
2. Writes them to that file and strips its inherited permissions, leaving read
   access to SYSTEM, Administrators and the service account only. `%PROGRAMDATA%`
   grants Users write by default, so without this step any account on the
   machine could read — or replace — the signing key.
3. Creates `C:\ProgramData\SKWAD\logs` and grants the service account write. A
   service has no console, so this file is the only place its output goes;
   it rolls daily.
4. Registers `SkwadBackend`, automatic start, running as
   `NT AUTHORITY\LocalService` — it reads one config, writes a log and listens
   on loopback, so it needs none of the authority `LocalSystem` would give it.

`skwad-backend status` reports whether it is installed, whether it is running,
and **which secrets actually resolve** — a half-filled config starts the service
and then fails, which otherwise shows up only as a service that will not stay
running. `uninstall` stops and deregisters it, and deliberately leaves the
signing key in place.

The binary is a real service, not a console app registered with `sc create`: it
connects to the Service Control Manager, registers a control handler and reports
`Running` only once the socket is accepting. That is what makes `Stop` return in
milliseconds instead of hanging for thirty seconds and being force-killed, and
what lets a dependent service order itself after it. The same binary still runs
in the foreground when a person starts it — it asks the SCM to dispatch and
falls back when told there is no service controller, so nothing about developing
against it changes.

On macOS and Linux there is no `install`: the binary is an ordinary foreground
process and launchd or systemd owns its lifecycle. It reads the same
`/etc/skwad/backend.env`.

## The Premiere Pro panel installs itself

Editors do not install a plugin. The app carries the panel and installs it on
first launch through Adobe's own installer agent, which ships with the Creative
Cloud desktop app — so a machine that can run Premiere can already install the
panel. Install SKWAD, open Premiere, and it is under **Window → Extensions →
SKWAD Collections**. No UXP Developer Tool, no developer mode, no Creative
Cloud Marketplace listing.

This is a per-user install, which is why it runs from the app rather than from
the installer: the Windows bundle is `perMachine` and runs elevated, so an NSIS
hook would install the panel into the administrator's profile instead of the
editor's. Running at launch puts it in the right profile by construction, and
covers macOS with the same code path. The details are in
[`apps/desktop/src-tauri/src/premiere_plugin.rs`](../apps/desktop/src-tauri/src/premiere_plugin.rs).

Nothing about it can fail an install or a launch — a machine with no Creative
Cloud or no Premiere logs a line and carries on. **Settings → Premiere Pro
panel** reports the state and offers a retry, which is where to look first if
an editor says the panel is missing.

Two things worth knowing before a wide rollout:

- **The package is unsigned.** Adobe's agent is the sanctioned route for
  internally distributed plugins, so this is the expected case rather than a
  workaround — but whether a given Creative Cloud version accepts an unsigned
  package silently has moved across releases. Check one machine before
  rolling out: install, open Premiere, look for the panel. If it is refused,
  sign the package once with the UXP Developer Tool's **Package** command and
  stage it with `node scripts/package-premiere-panel.mjs --from <signed.ccx>`;
  nothing else changes.
- **Updates ride along with the app.** The panel's own manifest version decides
  whether an installed copy is stale, so bumping it in
  `apps/premiere-panel/manifest.json` is what makes existing installs upgrade
  on next launch. Change the panel without bumping it and editors keep the old
  one.

## Path A — CI builds both installers (recommended)

`.github/workflows/release.yml` builds macOS (Apple Silicon) and Windows on
every `v*` tag and attaches both installers to a **draft** GitHub release.

```bash
# bump the version in apps/desktop/src-tauri/tauri.conf.json, package.json
# and Cargo.toml (workspace.package.version) so they agree, then:
git tag v1.0.0
git push origin v1.0.0
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
are present, builds for `aarch64-apple-darwin`, and prints where the `.dmg`
landed (`target/aarch64-apple-darwin/release/bundle/dmg/`). Expect 10–20
minutes on a cold build — ONNX Runtime and the webview crates dominate.

## Path C — Windows installer by hand

```bash
powershell -ExecutionPolicy Bypass -File scripts/fetch-models.ps1
npm run package:win
```

`package:win` builds `skwad-server`, stages it and `DirectML.dll`, then builds with
the config overlays. **Close the app first** — a running copy holds both
binaries, and the staging step cannot overwrite a file that is in use.
The installer lands in `target/release/bundle/nsis/` and is ~192 MB: the binary,
the DirectML redistributable and the two models. Verified contents of a build
made this way:

```
skwad-desktop.exe          6.0 MB   the shell: a window and a supervisor
skwad-server.exe          27.5 MB   the application itself
DirectML.dll            18.5 MB
models/det_10g.onnx     16.9 MB
models/w600k_r50.onnx  174.4 MB
```

The shell shrank from 32 MB to 6 MB when it stopped linking the database and
ONNX Runtime; that work moved into the server, not away.

`npm run tauri:build -w @skwad/desktop` on its own produces a ~32 MB installer
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
- `xattr -dr com.apple.quarantine "/Applications/SKWAD Media Organiser.app"`

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

## Handing the installer out from your own Apache server

The team installs from an Apache server on the office network instead of a
shared drive or a chat attachment. Apache only ever serves the **installer
file**: SKWAD is a desktop application whose UI talks to its Rust backend over
Tauri IPC, not HTTP, so there is nothing here to host as a website — a browser
build would render and then fail on every action.

**Apache must not host the shared library.** The index is a PostgreSQL database
— machines share it by connecting to the server, not by reaching a file over
WebDAV or HTTP. Point each machine at the server in **Settings → Library
database**, or write `database.json` into the library folder; see [one library,
shared across the network](shared-library.md).

### Publish a build

```powershell
npm run build   # produces target\release\bundle\nsis\*.exe

powershell -ExecutionPolicy Bypass -File scripts\publish-installer.ps1 `
  -Destination "C:\Apache24\htdocs\skwad" `
  -LibraryPath "\\STUDIO-PC\skwad-library"
```

The script copies the newest installer into the document root, writes a
`.sha256` beside it, keeps the last three builds (`-Keep`) and regenerates
`index.html`: a download page listing each build with its size and checksum,
the post-install steps, and the shared library path to enter. Send the team the
URL once — the same URL serves every future build.

### The virtual host

```apache
# httpd.conf: mod_headers and mod_authz_host must be loaded.
<VirtualHost *:80>
    ServerName skwad.local
    DocumentRoot "C:/Apache24/htdocs/skwad"

    <Directory "C:/Apache24/htdocs/skwad">
        Options -Indexes +FollowSymLinks
        AllowOverride None
        # Office network only — never expose the installer to the internet.
        Require ip 192.168.0.0/16
    </Directory>

    # Browsers must download the installer, not try to display it.
    AddType application/octet-stream .exe .dmg
    <FilesMatch "\.(exe|dmg)$">
        Header set Content-Disposition "attachment"
        Header set Cache-Control "public, max-age=0, must-revalidate"
    </FilesMatch>

    ErrorLog "logs/skwad-error.log"
    CustomLog "logs/skwad-access.log" common
</VirtualHost>
```

Adjust `Require ip` to your subnet. `must-revalidate` matters: without it a
workstation that downloaded the previous build can be served it again from
cache.

Add `skwad.local` to your DNS, or tell people the server's IP
(`http://192.168.1.10/`). On the Apache machine, allow port 80 through the
Windows firewall for the private network profile only.

### There is no auto-update

A new version is a new installer: publish it, tell the team, they download and
run it over the top. The library, settings and accounts live outside the
install folder, so nothing is lost. If that gets tedious, see [updating an
installed copy](#updating-an-installed-copy) for what an updater would need.

## What the recipient should know

- **Nothing is uploaded.** Everything runs locally; the source folder is only
  ever read from.
- **Their data lives in** `~/Library/Application Support/com.skwad.mediaorganiser`
  (macOS) or `%APPDATA%\com.skwad.mediaorganiser` (Windows): the index, the
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
