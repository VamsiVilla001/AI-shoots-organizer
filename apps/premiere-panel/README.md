# SKWAD Collections — Premiere Pro panel

A UXP panel that lists your SKWAD Media Organiser Collections inside Premiere
Pro and imports the selected one as a bin, referencing the original files on
your NAS or local disk. Nothing is copied.

It talks to a small local HTTP bridge that the desktop app runs on
`127.0.0.1:51823` while it's open (see
[`apps/desktop/src-tauri/src/premiere_api.rs`](../desktop/src-tauri/src/premiere_api.rs))
— so **SKWAD Media Organiser must be running** for the panel to work.

## Install (development / direct load, no marketplace listing)

1. Install Adobe's **UXP Developer Tool** (free, from the Creative Cloud
   desktop app or [Adobe's UXP developer site](https://developer.adobe.com/creative-cloud/console/)).
2. In Premiere Pro: **Edit → Preferences → Plugins → check "Enable developer
   mode"** (required on recent Premiere versions before it will load
   unsigned local plugins; restart Premiere if prompted).
3. Open UXP Developer Tool → **Add Plugin** → select this folder's
   `manifest.json`.
4. Click **Load** with Premiere Pro running. The panel appears under
   Premiere's **Window → Extensions → SKWAD Collections**.

No Adobe account, review, or fee is required for this — it's only needed if
you later choose to list the panel on the Creative Cloud Marketplace.

## First run

The panel needs the bridge's access token. With SKWAD Media Organiser
running, open `premiere-bridge.json` from its app data folder in a text
editor and copy the `token` value into the panel's setup screen:

- Windows: `%APPDATA%\com.skwad.mediaorganiser\premiere-bridge.json`
- macOS: `~/Library/Application Support/com.skwad.mediaorganiser/premiere-bridge.json`
- Linux: `~/.local/share/com.skwad.mediaorganiser/premiere-bridge.json`

The token is persisted by the app and remembered by the panel (via its local
storage), so you shouldn't need to paste it again — it survives restarts on
both sides. If the panel ever starts getting 401s (e.g. after the app's data
was reset), open `premiere-bridge.json` again and re-paste the current
token via **reconfigure** in the panel's toolbar.

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

## Packaging for distribution to a team

`UXP Developer Tool` → **Package** produces a `.ccx` file you can hand to
teammates directly (they load it the same way, via **Add Plugin**), or push
through Adobe's enterprise distribution if you manage Premiere via the Adobe
Admin Console. No Creative Cloud Marketplace listing is required for either.
