/**
 * SKWAD Collections panel — reads Collections from the SKWAD Media
 * Organiser desktop app over its loopback bridge (see
 * apps/desktop/src-tauri/src/premiere_api.rs) and imports the selected one
 * into Premiere as a bin, referencing the original files. Nothing is copied.
 */

const BRIDGE_PORT = 51823; // must match premiere_api.rs's BRIDGE_PORT and manifest.json
const SETTINGS_KEY = "skwad-premiere-panel-settings";

const els = {
  setup: document.getElementById("setup"),
  toolbar: document.getElementById("toolbar"),
  status: document.getElementById("status"),
  list: document.getElementById("list"),
  token: document.getElementById("token"),
  connect: document.getElementById("connect"),
  refresh: document.getElementById("refresh"),
  reconfigure: document.getElementById("reconfigure"),
};

let bridgeToken = null;
let pendingPollTimer = null;
const PENDING_POLL_MS = 3000;

function loadSavedSettings() {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY);
    return raw ? JSON.parse(raw) : null;
  } catch {
    return null;
  }
}

function saveSettings(settings) {
  try {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
  } catch {
    // Per-viewer convenience only; a failure here just means re-entering
    // the folder/token next time the panel opens.
  }
}

async function bridgeFetch(pathname) {
  // UXP's network sandbox rejects literal IP addresses — "localhost" is
  // required even though it resolves to the same loopback address.
  const response = await fetch(`http://localhost:${BRIDGE_PORT}${pathname}`, {
    headers: { Authorization: `Bearer ${bridgeToken}` },
  });
  if (!response.ok) {
    throw new Error(`SKWAD bridge returned ${response.status} for ${pathname}`);
  }
  return response.json();
}

function setStatus(message) {
  els.status.textContent = message;
}

function showSetup() {
  stopPendingPoll();
  els.setup.hidden = false;
  els.toolbar.hidden = true;
  els.list.innerHTML = "";
  const saved = loadSavedSettings();
  els.token.value = (saved && saved.token) || "";
}

async function connect() {
  const token = els.token.value.trim();
  if (!token) {
    setStatus("Paste the bridge token first — see premiere-bridge.json in SKWAD Media Organiser's app data folder.");
    return;
  }
  bridgeToken = token;

  saveSettings({ token });
  els.setup.hidden = true;
  els.toolbar.hidden = false;
  await loadCollections();
  startPendingPoll();
}

// Picks up "Send to Premiere" jobs queued from the app's own context menus
// (right-click a media file or a Collection) — those can only reach Premiere
// by landing here and being imported the same way a manual click would be.
// Only runs while this panel is open and connected, which is a real
// limitation worth keeping visible rather than pretending sends are instant.
function startPendingPoll() {
  stopPendingPoll();
  pendingPollTimer = setInterval(checkPendingJobs, PENDING_POLL_MS);
  checkPendingJobs();
}

function stopPendingPoll() {
  if (pendingPollTimer) clearInterval(pendingPollTimer);
  pendingPollTimer = null;
}

async function checkPendingJobs() {
  let jobs;
  try {
    jobs = await bridgeFetch("/pending");
  } catch {
    // Silent: this polls every few seconds, and a transient failure (e.g.
    // the app briefly restarting) shouldn't overwrite whatever the status
    // line is currently showing.
    return;
  }
  for (const job of jobs) {
    setStatus(`Importing "${job.label}" sent from SKWAD Media Organiser…`);
    try {
      const filePaths = job.files.map((file) => file.path);
      // job.bin is set for a Collection send (its files belong together as
      // a set) and omitted for an ad-hoc file/selection send, which lands in
      // the project root instead of getting its own auto-named bin.
      const imported = await importIntoPremiere(job.bin, filePaths);
      setStatus(`Imported ${imported} of ${filePaths.length} file(s)${job.bin ? ` into "${job.bin}"` : ""}.`);
    } catch (error) {
      setStatus(`Import of "${job.label}" failed: ${error.message}`);
    }
  }
}

async function loadCollections() {
  setStatus("Loading collections…");
  els.list.innerHTML = "";
  try {
    const collections = await bridgeFetch("/collections");
    if (collections.length === 0) {
      setStatus("No collections found. Create one in SKWAD Media Organiser first.");
      return;
    }
    setStatus("");
    for (const collection of collections) {
      els.list.appendChild(renderCollection(collection));
    }
  } catch (error) {
    setStatus(`Could not reach SKWAD Media Organiser: ${error.message}. Is it running?`);
  }
}

function renderCollection(collection) {
  const row = document.createElement("div");
  row.className = "collection";

  const meta = document.createElement("div");
  meta.className = "meta";
  const name = document.createElement("div");
  name.className = "name";
  name.textContent = collection.name;
  const project = document.createElement("div");
  project.className = "project";
  project.textContent = `${collection.projectName} · ${collection.mediaCount} file${collection.mediaCount === 1 ? "" : "s"}`;
  meta.append(name, project);

  const button = document.createElement("button");
  button.textContent = "Import";
  button.disabled = collection.mediaCount === 0;
  button.addEventListener("click", () => importCollection(collection, button));

  row.append(meta, button);
  return row;
}

async function importCollection(collection, button) {
  button.disabled = true;
  const originalLabel = button.textContent;
  button.textContent = "Importing…";
  try {
    const files = await bridgeFetch(`/collections/${encodeURIComponent(collection.id)}/media`);
    const filePaths = files.map((file) => file.path);
    const imported = await importIntoPremiere(collection.name, filePaths);
    setStatus(`Imported ${imported} of ${filePaths.length} file(s) into "${collection.name}".`);
  } catch (error) {
    setStatus(`Import failed: ${error.message}`);
  } finally {
    button.disabled = false;
    button.textContent = originalLabel;
  }
}

// Not a global `app` (that's the CEP/ExtendScript model) — Premiere's UXP
// scripting surface is a separate module. Pattern confirmed against Adobe's
// own uxp-premiere-pro-samples repo (sample-panels/premiere-api/src/projectPanel.ts).
const ppro = require("premierepro");

async function createBin(project, name) {
  const rootItem = await project.getRootItem();
  project.lockedAccess(() => {
    project.executeTransaction((compoundAction) => {
      compoundAction.addAction(rootItem.createBinAction(name, true));
    }, "Create Bin");
  });
  const items = await rootItem.getItems();
  return items.find((item) => item.name === name) || null;
}

/**
 * @param binName Name of the bin to create/reuse, or a falsy value to import
 *   straight into the project root (no bin at all — the right call for a
 *   one-off file or an ad-hoc multi-select, which shouldn't force a new bin
 *   the user didn't ask for).
 */
async function importIntoPremiere(binName, filePaths) {
  if (filePaths.length === 0) return 0;
  const project = await ppro.Project.getActiveProject();
  if (!project) {
    throw new Error("no project is open in Premiere");
  }
  const bin = binName ? await createBin(project, binName) : null;
  const ok = await project.importFiles(filePaths, true, bin, false);
  return ok ? filePaths.length : 0;
}

els.connect.addEventListener("click", connect);
els.refresh.addEventListener("click", loadCollections);
els.reconfigure.addEventListener("click", showSetup);

showSetup();
const saved = loadSavedSettings();
if (saved && saved.token) {
  connect();
}
