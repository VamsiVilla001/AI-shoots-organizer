# SKWAD Project Workspace v2 plan

**Date:** 12 September 2026  
**Scope:** Simplify the product around projects and collections while preserving the working scan and video-processing pipeline.

## Product decision

Keep the three primary destinations:

1. **Collections** — projects, nested collections, collaboration and output.
2. **Media Processing** — import, processing status, processed imports, tagging and automatic tags.
3. **Settings** — application, AI, storage and catalogue-exchange settings.

Keep **Profile** on the account control at the bottom of the sidebar. It is an account destination, not an application setting.

Do not change the scanner, job queue, video sampling, recognition pipeline or recovery behavior. The work is in the project model, permissions, information architecture and the paths into the existing editing tools.

## Audit evidence

| Step | Evidence | General health | Finding |
| --- | --- | --- | --- |
| 1 | [New Projects dashboard](workflow-audit/01-projects.png) | Needs structural work | The screen is visually quiet and project-first, but every card is only a local project. There is no Personal, Shared or Organisation partition, owner, access level, recent activity or durable project state. |
| 2 | [New Media Processing](workflow-audit/02-processing.png) | Functionally strong, visually too tall | Live controls are in the correct area, but the expanded diagnostic panel pushes Media library, Processed jobs, Tag media and Auto tags below the first viewport. |
| 3 | [Classic Recent Shoots](workflow-audit/03-classic-shoots.png) | Feature-rich, fragmented | Classic exposes useful operational controls such as resume, export, Delete Index and bulk clearing, but its shoot-centric navigation splits one job across many top-level destinations. |

The preview could not render the deeper Classic Groups screen because its browser mock does not implement every native command used by that screen. Detailed parity findings below therefore combine the captured navigation evidence with source-code inspection and the repository's current-application documentation.

## Target information architecture

### Collections

Collections opens to **Projects** with three views:

- **Personal** — projects owned by the signed-in user. Private by default.
- **Shared with me** — projects where the user is an invited editor or viewer.
- **Organisation** — projects published for everyone in the organisation.

Each project card should show name, project type, visibility, owner, last updated time, collection count and media count. Show an active-processing indicator only when an import linked to the project is still running. Give each card a visible overflow button with Open, Share, Edit details, Archive and Delete. Right-click can remain as a shortcut, but cannot be the only way to discover management actions.

Opening a project should keep one focused workspace:

- breadcrumb and project name;
- access badge and member avatars/count;
- Share, Add media and Export actions;
- nested collection browser;
- project-level search and filters;
- a compact activity area for recent imports, tags and exports.

Collection folders inherit project access by default. A later release may support restricted collections, but inheritance should be the first implementation because it is easier to understand and audit.

### Media Processing

Use this sequence without changing the processing engine:

1. **Add media** asks for a Name and source folder. Project assignment is optional.
2. **Processing now** shows a compact row for every active import: name, phase, percentage, estimated time and Pause/Resume/Cancel. Full pipeline and CPU/GPU telemetry stays behind **View details**.
3. **Media library** contains every indexed import and remains universal.
4. **Processed jobs** is processing history and troubleshooting, not a second media library.
5. **Tag media** shows human-confirmed people and lets the user add selected or all matching media to a collection.
6. **Auto tags** shows recognised people, unknown groups and generated albums. Every result needs **Review** and **Add to collection** actions.

The add-to-collection dialog should always use the same order: **Project → Collection location → Add**. It should remember the most recent project and location for the session.

### Settings and Profile

Settings keeps AI/runtime controls, media/cache management and catalogue exchange. Destructive index controls from Classic should move to **Settings → Storage & indexed data**, with counts and explicit confirmation.

Profile remains under the account control and contains identity/account information only.

## Project and sharing model

The current project records live in webview `localStorage` and contain only a name, type and references to Classic groups. That is insufficient for Personal and Shared Projects.

Persist the following in SQLite or the product's future sync service:

- `projects`: id, name, type, owner, visibility, status, cover, created and updated timestamps;
- `project_members`: project, user, role and invitation state;
- `collections`: id, project, parent, name, notes, sort order and timestamps;
- `collection_media`: collection and media reference, with provenance such as manual, tag, auto tag or Classic group;
- `project_activity`: import, membership, collection and export events needed for useful history.

Use three roles: **Owner**, **Editor** and **Viewer**. Use three visibility states: **Private**, **Invited people** and **Organisation**. The owner can manage access and delete; editors can organise collections; viewers can browse and export. Media access must never become broader than the source or project permission that granted it.

Migrate existing local projects once, preserving IDs and nested collection structure. Keep Classic groups linked during migration so the current media remains available. Record migration completion in durable storage rather than repeatedly reading localStorage.

## Classic-to-project feature inventory

| Classic capability | New home | Status / required change |
| --- | --- | --- |
| Create and resume shoot | Media Processing | Retained. Keep backend behavior unchanged. |
| Processing stages, pause, resume, cancel, reanalyse, recovery | Processing now / Processed jobs | Retained. Collapse detailed diagnostics by default. |
| Delete one index and bulk clear scanned data | Settings → Storage & indexed data | Missing from new workspace. Relocate with clear source-file safety text. |
| Sort media into groups; add, move and remove selected files | Project collection browser | Partial. Preserve add-versus-move semantics and multi-select. |
| Group rename, output folder name, notes, empty and delete | Collection settings | Partial. Rename/remove exist; folder name, notes and empty are missing. |
| Seed groups from AI albums | Auto tags → Add to collection | Partial. Existing album-to-group action is import-centric and does not complete the project flow. |
| Reusable player profiles | Project people panel or Tag media → Manage person | Buried. Surface rename, team, merge, delete recognition data and delete profile. |
| Review suggestions, unknown, confirmed and all faces | Auto tags → Review queue | Buried. Preserve confidence order, bulk confirm/reject/reassign and Not a face. |
| Name unknown clusters and improve future recognition | Auto tags | Retained through Classic component; needs project language and direct collection action. |
| AI albums by player, team, multiple people and person count | Auto tags | Retained through Classic component; needs direct project/collection destination. |
| Regenerate albums | Auto tags details | Retain as a secondary action. |
| Ratings, picks, best shots and duplicate filters | Project/collection media toolbar | Partial. Verify that pick/reject filtering is exposed as clearly as rating, best-shot and duplicate filtering. |
| Copy & Organise preview and destination safeguards | Project or collection Export | Partial. Preserve preview, conflict policy, metadata, manifest, cancellation and history. |
| Export selected players/groups | Project or collection Export | Partial. Add project-wide and multi-import export; the current project path exports one source at a time. |
| Shared encrypted `.skwad` catalogues | Settings → Catalogue exchange | Retained, but it is offline catalogue transfer and must not be presented as Shared Projects. |
| Profile | Account control → Profile | Correctly separated. |
| AI/runtime/storage configuration | Settings | Retained. |

## Missing project-management functions

The following are required before Project workspace can replace Classic:

- durable project persistence and one-time migration;
- Personal, Shared with me and Organisation project views;
- owner/member display, invitations, roles and access changes;
- visible project and collection action menus that work by mouse and keyboard;
- archive/restore and safe delete behavior;
- move, copy and duplicate collections within or across projects;
- project-wide search across nested collections, media, people and tags;
- project and collection notes, cover image and meaningful updated timestamps;
- direct add-to-collection from Media library, Tag media and Auto tags;
- a unified review queue for suggestions and unknown people;
- project-level export across multiple imports, with export history;
- handling for deleted or unavailable source indexes instead of leaving silent broken references;
- empty, loading, error and permission-denied states for every project view.

## Delivery plan

### Phase 1 — durable projects and clear ownership

- Add persistent projects, collections, memberships and visibility.
- Migrate existing local project metadata without changing Classic groups or source media.
- Add Personal, Shared with me and Organisation views.
- Add visible project actions, Share/Manage access, Archive and safe Delete.
- Keep the Classic switch available throughout the phase.

**Exit criteria:** projects survive restart and migration; two users/roles resolve to the correct visible projects; viewers cannot edit; deleting project organisation does not touch source files or indexes.

### Phase 2 — one collection workflow

- Standardise Add to collection across Media library, Tag media and Auto tags.
- Add move/copy/duplicate, notes and ordering for nested collections.
- Add project search and a small project overview.
- Replace the expanded live panel with a compact default and optional diagnostics.

**Exit criteria:** a user can import once, review tags, add results to any permitted collection and find them again without entering Classic.

### Phase 3 — recover essential Classic capability

- Surface Review as a queue launched from Auto tags and relevant project states.
- Surface reusable person management from Tag media.
- Complete project-wide, multi-import Export while preserving Classic safeguards and history.
- Move index/cache cleanup into Settings.
- Add collection notes, output-folder naming and empty-collection controls where users still need them.

**Exit criteria:** every retained Classic capability in the inventory has a tested new-workspace route; no workflow depends on knowing the former sidebar structure.

### Phase 4 — replacement readiness

- Test keyboard access, focus states, permission boundaries, large libraries and missing-source recovery.
- Add migration rollback/diagnostics and telemetry for abandoned workflows.
- Run side-by-side acceptance with real tournament, sports and wedding projects.
- Remove the Classic switch only after the parity checklist and real-library migration pass.

## Recommended first build slice

Build Phase 1 before polishing more cards or adding more tabs. The smallest useful slice is:

1. persistent project and membership schema;
2. Personal / Shared with me / Organisation project views;
3. project Create, Share, Edit, Archive and Delete;
4. migration of current local projects;
5. permission-aware project opening and collection editing.

This slice fixes the main product problem: SKWAD will treat a tournament, wedding or event as the durable unit of work, while imports remain reusable media sources and collections remain flexible views inside that project.
