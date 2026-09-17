/**
 * SKWAD Collections panel — reads Collections from the SKWAD Media
 * Organiser desktop app over its loopback bridge (see
 * apps/desktop/src-tauri/src/premiere_api.rs) and imports the selected one
 * into Premiere as a bin, referencing the original files. Nothing is copied.
 */

const BRIDGE_PORT = 51823; // must match premiere_api.rs's BRIDGE_PORT and manifest.json
// Fixed shared secret, not a per-install one — see the rationale in
// premiere_api.rs's module doc comment. Must match BRIDGE_TOKEN there exactly.
const BRIDGE_TOKEN = "skwad-premiere-bridge-v1";

const els = {
  toolbar: document.getElementById("toolbar"),
  signedInAs: document.getElementById("signedInAs"),
  status: document.getElementById("status"),
  tabs: document.getElementById("tabs"),
  breadcrumb: document.getElementById("breadcrumb"),
  list: document.getElementById("list"),
  refresh: document.getElementById("refresh"),
};

let pendingPollTimer = null;
const PENDING_POLL_MS = 3000;

// Project/Collection navigation state — mirrors the app's own Collections
// screen (apps/desktop/src/projectWorkspace/nestedCollections.tsx) so
// browsing here feels the same as browsing there.
const VIEWS = [
  ["organisation", "Organisation"],
  ["shared", "Shared"],
  ["personal", "Personal"],
  ["archived", "Archived"],
];
let allProjects = [];
let currentView = "personal";
let currentProjectId = null;
let currentCollectionId = null; // null = the project's root level

// --- ported 1:1 from nestedCollections.tsx so the panel sorts projects into
// the same tabs and labels them the same way the app does ---
function inView(project, view) {
  if (view === "archived") return project.status === "archived" && project.accessRole === "owner";
  if (project.status === "archived") return false;
  if (project.visibility === "organisation") return view === "organisation";
  if (view === "personal") return project.accessRole === "owner";
  if (view === "shared") return project.accessRole !== "owner";
  return false;
}
function accessLabel(project) {
  if (project.visibility === "organisation") return "Organisation";
  if (project.accessRole !== "owner") return `Shared · ${project.accessRole}`;
  if (project.visibility === "invited") return "Invited people";
  return "Private";
}
function childrenOf(project, parentId) {
  return project.collections
    .filter((c) => c.parentId === parentId)
    .sort((a, b) => a.sortOrder - b.sortOrder || a.name.localeCompare(b.name));
}
function collectionTrail(project, collectionId) {
  const trail = [];
  const seen = new Set();
  let current = project.collections.find((c) => c.id === collectionId);
  while (current && !seen.has(current.id)) {
    seen.add(current.id);
    trail.unshift(current);
    current = current.parentId ? project.collections.find((c) => c.id === current.parentId) : undefined;
  }
  return trail;
}

async function bridgeFetch(pathname) {
  // UXP's network sandbox rejects literal IP addresses — "localhost" is
  // required even though it resolves to the same loopback address.
  const response = await fetch(`http://localhost:${BRIDGE_PORT}${pathname}`, {
    headers: { Authorization: `Bearer ${BRIDGE_TOKEN}` },
  });
  if (!response.ok) {
    const body = await response.text().catch(() => "");
    const error = new Error(body || `SKWAD bridge returned ${response.status} for ${pathname}`);
    error.status = response.status;
    throw error;
  }
  return response.json();
}

function setStatus(message) {
  els.status.textContent = message;
}

async function start() {
  els.toolbar.hidden = false;
  currentProjectId = null;
  currentCollectionId = null;
  await loadProjects();
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

async function loadProjects() {
  setStatus("Loading projects…");
  els.list.innerHTML = "";
  try {
    const response = await bridgeFetch("/projects");
    allProjects = response.projects;
    els.signedInAs.textContent = `Signed in as ${response.email}`;
    setStatus("");
    render();
  } catch (error) {
    els.signedInAs.textContent = "";
    setStatus(
      error.status
        ? `SKWAD Media Organiser: ${error.message}`
        : `Could not reach SKWAD Media Organiser: ${error.message}. Is it running?`,
    );
  }
}

function render() {
  if (currentProjectId === null) {
    els.tabs.hidden = false;
    els.breadcrumb.hidden = true;
    renderTabs();
    renderProjectsList();
    return;
  }
  const project = allProjects.find((p) => p.id === currentProjectId);
  if (!project) {
    // The project disappeared (e.g. access revoked) since it was opened.
    currentProjectId = null;
    currentCollectionId = null;
    render();
    return;
  }
  els.tabs.hidden = true;
  els.breadcrumb.hidden = false;
  renderBreadcrumb(project);
  renderCollectionsList(project);
}

function renderTabs() {
  els.tabs.innerHTML = "";
  for (const [id, label] of VIEWS) {
    const count = allProjects.filter((p) => inView(p, id)).length;
    const button = document.createElement("button");
    button.setAttribute("role", "tab");
    button.setAttribute("aria-selected", String(id === currentView));
    button.innerHTML = `${label}<span>${count}</span>`;
    button.addEventListener("click", () => {
      currentView = id;
      render();
    });
    els.tabs.appendChild(button);
  }
}

function renderProjectsList() {
  els.list.innerHTML = "";
  const visible = allProjects.filter((p) => inView(p, currentView));
  if (visible.length === 0) {
    setStatus("No projects here. Create one in SKWAD Media Organiser first.");
    return;
  }
  setStatus("");
  for (const project of visible) {
    const roots = childrenOf(project, null);
    els.list.appendChild(
      renderNode({
        name: project.name,
        detail: `${accessLabel(project)} · ${project.ownerEmail}`,
        meta: `${roots.length} main collection${roots.length === 1 ? "" : "s"}`,
        onOpen: () => {
          currentProjectId = project.id;
          currentCollectionId = null;
          render();
        },
      }),
    );
  }
}

function renderBreadcrumb(project) {
  els.breadcrumb.innerHTML = "";
  const crumb = (label, onClick) => {
    if (onClick) {
      const button = document.createElement("button");
      button.textContent = label;
      button.addEventListener("click", onClick);
      els.breadcrumb.appendChild(button);
    } else {
      const span = document.createElement("span");
      span.textContent = label;
      els.breadcrumb.appendChild(span);
    }
  };
  const sep = () => {
    const span = document.createElement("span");
    span.textContent = "/";
    els.breadcrumb.appendChild(span);
  };

  crumb("Projects", () => {
    currentProjectId = null;
    currentCollectionId = null;
    render();
  });
  sep();
  if (currentCollectionId === null) {
    crumb(project.name, null);
    return;
  }
  crumb(project.name, () => {
    currentCollectionId = null;
    render();
  });
  const trail = collectionTrail(project, currentCollectionId);
  trail.forEach((node, index) => {
    sep();
    const isLast = index === trail.length - 1;
    crumb(
      node.name,
      isLast
        ? null
        : () => {
            currentCollectionId = node.id;
            render();
          },
    );
  });
}

function renderCollectionsList(project) {
  els.list.innerHTML = "";
  const current = currentCollectionId === null ? null : project.collections.find((c) => c.id === currentCollectionId);

  if (current && current.mediaCount > 0) {
    const row = document.createElement("div");
    row.className = "node";
    const meta = document.createElement("div");
    meta.className = "meta";
    meta.innerHTML = `<div class="name">Media in this collection</div><div class="detail">${current.mediaCount} file${current.mediaCount === 1 ? "" : "s"}</div>`;
    const actions = document.createElement("div");
    actions.className = "actions";
    const button = document.createElement("button");
    button.className = "primary";
    button.textContent = "Import";
    button.addEventListener("click", () => importCollection(current, button));
    actions.appendChild(button);
    row.append(meta, actions);
    els.list.appendChild(row);
  }

  const children = childrenOf(project, currentCollectionId);
  if (children.length === 0 && !(current && current.mediaCount > 0)) {
    setStatus("This collection is empty.");
  } else {
    setStatus("");
  }
  for (const collection of children) {
    const subCount = childrenOf(project, collection.id).length;
    const detail = subCount > 0 ? `${subCount} collection${subCount === 1 ? "" : "s"}` : `${collection.mediaCount} file${collection.mediaCount === 1 ? "" : "s"}`;
    const node = renderNode({
      name: collection.name,
      detail,
      onOpen: () => {
        currentCollectionId = collection.id;
        render();
      },
    });
    if (collection.mediaCount > 0) {
      const button = document.createElement("button");
      button.className = "primary";
      button.textContent = "Import";
      button.addEventListener("click", () => importCollection(collection, button));
      node.querySelector(".actions").appendChild(button);
    }
    els.list.appendChild(node);
  }
}

/** A clickable name/detail row with an empty `.actions` slot callers can add buttons into. */
function renderNode({ name, detail, meta, onOpen }) {
  const row = document.createElement("div");
  row.className = "node";

  const metaEl = document.createElement("div");
  metaEl.className = "meta";
  metaEl.addEventListener("click", onOpen);
  const nameEl = document.createElement("div");
  nameEl.className = "name";
  nameEl.textContent = name;
  metaEl.appendChild(nameEl);
  if (detail) {
    const detailEl = document.createElement("div");
    detailEl.className = "detail";
    detailEl.textContent = meta ? `${detail} · ${meta}` : detail;
    metaEl.appendChild(detailEl);
  }

  const actions = document.createElement("div");
  actions.className = "actions";
  const open = document.createElement("button");
  open.textContent = "Open";
  open.addEventListener("click", onOpen);
  actions.appendChild(open);

  row.append(metaEl, actions);
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

els.refresh.addEventListener("click", loadProjects);

start();
