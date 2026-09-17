# SKWAD Collections — Premiere Pro panel

A UXP panel that browses your SKWAD Media Organiser Projects and Collections
inside Premiere Pro — the same Personal / Shared with me / Organisation /
Archived tabs and nested folder structure as the app's own Collections
screen — and imports a Collection's media as a bin, referencing the original
files on your NAS or local disk. Nothing is copied.

It talks to a small local HTTP bridge that the desktop app runs on
`127.0.0.1:51823` while it's open (see
[`apps/desktop/src-tauri/src/premiere_api.rs`](../desktop/src-tauri/src/premiere_api.rs))
— so **SKWAD Media Organiser must be running** for the panel to work.

## How it reaches users

Nobody installs this by hand. The desktop app ships the panel inside its own
installer and installs it on first launch, so an editor installs SKWAD Media
Organiser, opens Premiere, and finds the panel under **Window → Extensions →
SKWAD Collections**. No UXP Developer Tool, no developer mode, no Creative
Cloud Marketplace listing, no Adobe account.

That works because a UXP plugin cannot be installed by copying a folder into
place — Premiere reads a registry of installed plugins that only Adobe's own
installer agent writes — and that agent, `UnifiedPluginInstallerAgent`, ships
with the Creative Cloud desktop app and is therefore already on every machine
that runs Premiere. See
[`apps/desktop/src-tauri/src/premiere_plugin.rs`](../desktop/src-tauri/src/premiere_plugin.rs)
for why the install runs at launch rather than from the installer.

**Settings → Premiere Pro panel** in the desktop app reports whether the panel
is installed and offers a retry — the first place to look when an editor says
it isn't there.

### Building the package

```bash
npm run package:premiere-panel
```

Writes `apps/desktop/src-tauri/resources/premiere-panel/skwad-collections.ccx`
(a `.ccx` is an ordinary zip with `manifest.json` at its root) and stages the
manifest beside it, which is how the app tells whether an installed panel is
current. `npm run build` runs this first, so a release always ships the panel
as it stands in this folder.

The package is **unsigned**. If a Creative Cloud version refuses it, sign it
once with the UXP Developer Tool's **Package** command and stage that instead —
nothing downstream changes, because both are just a file at the same path:

```bash
node scripts/package-premiere-panel.mjs --from path/to/signed.ccx
```

## Developing the panel

To iterate on the panel itself, load it directly rather than rebuilding and
reinstalling the app each time:

1. Install Adobe's **UXP Developer Tool** (free, from the Creative Cloud
   desktop app or [Adobe's UXP developer site](https://developer.adobe.com/creative-cloud/console/)).
2. In Premiere Pro: **Edit → Preferences → Plugins → check "Enable developer
   mode"** (restart Premiere if prompted).
3. UXP Developer Tool → **Add Plugin** → select this folder's `manifest.json`.
4. Click **Load** with Premiere Pro running.

This is a developer-machine workflow only; none of it reaches users. A panel
loaded this way talks to the same bridge as an installed one, so the two are
interchangeable for testing — but having both at once is confusing, so remove
the installed copy first (**Creative Cloud → Stock & Marketplace → Manage
plugins**) if the wrong one keeps loading.

## First run

Nothing to configure. The panel connects automatically as soon as it loads —
no token to copy, no setup screen. It shows "Signed in as …" once connected,
reflecting whichever SKWAD account is signed into the desktop app.

That's possible because the bridge's access token is a fixed value baked
into both `apps/desktop/src-tauri/src/premiere_api.rs` and this panel's
`main.js`, not a random per-install secret to discover and paste. See the
comment at the top of `premiere_api.rs` for the reasoning — short version:
the bridge only ever listens on loopback (unreachable from outside this
machine), and *who* you are is answered by the desktop app's own signed-in
session, not by the token, so a fixed constant protects exactly as much as
a random one would have.

If the status line says it can't reach SKWAD Media Organiser, make sure the
app is running. If it says you need to sign in, that's the desktop app's own
SKWAD account — sign in there and hit **Refresh** here.

## Sending from the app itself

Right-clicking a media file or a Collection inside SKWAD Media Organiser and
choosing **Send to Premiere** works the same way, but indirectly: the app
can't call into Premiere directly, so it queues the request on the bridge,
and this panel picks it up on its next poll (every few seconds) and does the
actual import. That means **this panel has to be open and connected** for a
send from the app to arrive; if it isn't, the request just waits until the
panel is opened.

A Collection send creates a bin named after the Collection, same as clicking
Import here. A single file or an ad-hoc multi-select does not — those land
directly in the project root, since forcing a new auto-named bin per send
would clutter the project for no benefit; bin them yourself in Premiere if
you want to.

## If Import throws an error

Premiere's UXP scripting API for creating a bin (`createBin`) has changed
shape across Premiere versions more than once; `importFiles` has not. If
import fails with something bin-related, check the current pattern in
Adobe's [`uxp-premiere-pro-samples`](https://github.com/AdobeDocs/uxp-premiere-pro-samples)
repo and adjust the `createBin` function in `main.js` — nothing else in the
panel needs to change.
