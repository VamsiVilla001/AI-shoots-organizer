# Project workspace: parallel UI, first iteration

The desktop app now offers Project workspace and Classic. The experience switch
is remembered on the device. No Classic screen or backend table was removed.

The new shell uses the existing SKWAD v0.73 stylesheet tokens, Anybody headings,
Manrope text, JetBrains Mono labels, and the supplied SKWAD logo. Its additional
CSS is scoped to `pw-*` classes. It follows the supplied collection-grid reference
with project cards, a compact three-item sidebar, readable headings, and details
disclosed only when needed.

## Feature comparison

| Classic capability | New location | First-iteration status |
| --- | --- | --- |
| Recent Shoots / add folder | Media Processing → Media library / Add media | New interface, existing scanner |
| All previously indexed media | Media Processing → All imported media | Cross-import library, paginated |
| Sort into Groups | Media selection → Create collection; Identify & organise → Manual grouping | New collection publishing plus existing advanced sorting |
| Players | Find a person filter; Identify & organise → Manage people | Existing editor retained |
| AI Albums / sampled face naming | Identify & organise → Sampled faces & AI suggestions | Existing editor retained |
| Review / correction / manual face boxes | Identify & organise → Review face matches; shared media viewer | Existing editor and viewer retained |
| Ratings, picks, best shots, duplicates | Collection/library media grid and More filters | Existing editorial commands and shortcuts |
| Shortcut Export | Open a collection → Export collection | Group preselected; multiple imports exported one source at a time |
| Queue, pause, resume, retry, telemetry | Media Processing → live panel above tabs | Active jobs stay visible with details and controls expanded |
| Completed processing history | Media Processing → Processed jobs | Only completed imports appear in this history |
| Tagged people and their media | Media Processing → Tag media | Names assigned during review appear automatically; all tagged files can be added directly or selected individually for a project collection |
| Automatic face grouping | Media Processing → Auto tags | Choose a processed import to review recognised people, unknown groups, and AI albums |
| Shared Catalogues | Settings → Catalogue exchange | Existing encrypted catalogue workflow, not project ACLs |
| Profile | Sidebar profile button → Profile | Separate account editor, outside application settings |
| AI/runtime/storage settings | Settings → General | Existing editor |
| Delete index / bulk clear scanned data | Classic → Shoots | Intentionally retained in Classic during comparison |

## What is implemented

- Create, rename, search, and delete projects. Deleting a project removes its
  organisation only; indexed media and original groups stay in the library.
- Media Processing shows each imported folder as a collection card. The card name
  is the `Name` entered during import; opening it reveals its reusable media.
- Choosing a project type creates a useful starter collection structure. Esports,
  sports, and wedding templates include nested collections; `Other` starts empty.
- Collections can contain other collections at any depth. Existing project
  metadata is migrated to root-level collections automatically.
- Right-clicking a collection folder opens its settings menu: open, rename,
  create or link a collection inside it, and remove it from the project.
- Link existing backend groups into projects without moving or recreating media.
- Select files across imports and create a collection in a new or existing project.
- A collection can reference one backend group per source import. Each group's
  memberships remain in the existing SQLite database and are visible in Classic.
- New collection creation checks for same-named groups before writing because
  the existing `create_group` command uses get-or-create semantics. A conflicting
  name is rejected rather than silently modifying a Classic group.
- Multi-source creation retains successful groups on partial failure. Retry uses
  those IDs; it does not delete source data or previously created groups.
- Remove a collection from a project without deleting its original groups or files.
- Project metadata is stored separately under
  `skwad.project-workspace.v1.<account>` in webview localStorage. The key contains
  only project names/types and group references. Media is not copied into it.
- Read errors do not overwrite malformed saved project metadata. Storage write
  errors are surfaced rather than reporting a successful save.

## Deliberate boundaries

This is the first side-by-side implementation, not the final replacement.

- Project metadata is local to this device/account namespace, not synced or part
  of a `.skwad` catalogue backup. It is not an access-control boundary.
- Private, specific-person, and organisation sharing still require backend project
  ownership, membership and ACL implementation. The UI says **Local project** and
  does not offer misleading public/private switches.
- Universal media currently means the existing local indexed library. No new
  organisation-wide access or source-media permissions are granted.
- Import currently selects folders, matching the existing backend API. Individual
  file imports need a separate backend change.
- The advanced face, sorting, settings, and export screens still use their Classic
  components. Their detailed layouts and terminology are the next design pass.
- Existing groups are linked explicitly; there is no guessed automatic migration
  of shoots into projects. Deleting a source index in Classic invalidates its
  project references; it does not delete project metadata.
- New groups reflect selections at creation time. They do not automatically add
  future appearances after subsequent recognition runs.

## Review safely

The normal desktop entry uses real authenticated Tauri commands. Run the desktop
app normally to compare against its existing data.

For UI-only review, the Vite development server serves
`/workspace-preview.html`. This separate entry uses synthetic in-memory IPC data
and browser-local sample project/group metadata, with the account label
**Sample workspace / Sample data only**. It has no real media or thumbnails.
Native imports, face recognition, settings changes, and file exports are not
simulated as successful operations. It is not included in the production build
and refuses to run in Tauri or outside development mode.

## Validation

- `npm run typecheck` and `npm run web:build` pass.
- Browser sample-data checks: project creation; existing group linking; collection
  creation from two imports; browsing both sources; opening export with only the
  selected collection checked; switching to Classic and back.
- Screenshots inspected for desktop layout. Sample cards intentionally have no
  photographic covers; real groups use existing media thumbnails.
- Native scanning, recognition, permission enforcement and filesystem export
  were not executed against the user's library during this UI change.
