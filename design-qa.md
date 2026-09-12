# Project workspace — first iteration QA

final result: passed (parallel UI review scope)

This is an intentional redesign using the user's collection-grid screenshot as
layout guidance and the repository's SKWAD v0.73 styles as the visual system. It
is not a pixel-for-pixel clone of the Tesseract reference.

## Inspected views

- Projects dashboard at 1440 × 960: three-item navigation, clear heading and
  primary action, readable cards, search, library entry, and Classic switch.
- Collection media at desktop size: source switching, filtered media, export
  preselection. Removed redundant project actions from the collection heading
  after reviewing the initial screenshot; removal now lives under options.
- Live processing at 1440 × 960: active work appears directly below the Media
  Processing header with details and controls expanded. Processed jobs contains
  only completed imports and keeps its summary collapsed.
- Auto tags follows Tag media. It lists only completed imports, opens the existing
  AI Albums workflow, and keeps active processing visible above the tabs.
- Media library at 1260 × 620: imports appear directly as SKWAD collection cards;
  no media-source dropdown is present. The separate Search all media path retains
  cross-import person and file discovery.
- Tag media at 1260 × 620: user-assigned person tags appear as a searchable list.
  Opening a tag shows recognised media from every import and exposes the existing
  select-and-create-collection flow.
- Used `Add to collection` directly from a tag, chose a nested destination, and
  confirmed all tagged media was published into that project collection.
- Opened folder settings with a right-click and verified open, rename, create
  inside, add existing inside, and remove actions. Rename opened the expected
  collection-name dialog.
- Confirmed Settings contains only General and Catalogue exchange. The sidebar
  account card now opens a separate Profile screen and receives its own active
  state.
- Right-clicked a project card and confirmed Open project, Edit details, and
  Delete project. Project settings supports name/type changes and explains that
  deletion keeps indexed media and original groups.
- Nested collection path: opened a main collection, saw its direct child beside
  its own media, created another child, and confirmed the breadcrumb represented
  three collection levels without flattening the hierarchy.
- Project templates: changed the project type and confirmed its preview updated,
  then created an esports project and verified the generated Teams, Players, MVP
  Videos, and Match Highlights structure. Teams contained Team Entry, Team Reveal,
  and WWCD Moments. `Other` correctly previewed an empty starting structure.
- Collection creation inside another collection is labelled `New collection` in
  both the primary action and dialog.
- Narrow layout at 760 × 900: navigation moves above content and cards reflow;
  no sideways overflow was visible in the inspected screenshot.

Screenshots are in [docs/project-workspace-review](docs/project-workspace-review).
Files marked `initial` show the earlier inspection before the secondary-action
cleanup. The sample preview intentionally has no real media imagery.

## Interaction checks

- Created a project and linked an existing sample group; neither source group
  nor its media was removed.
- Selected six sample files across two imports and created a collection in an
  existing project. Opened it and switched between its two media sources.
- Opened export from that collection: its group was checked and the unrelated
  group was unchecked; export-all was off.
- Switched to Classic and back; original nine destinations remain available and
  project metadata remains available.
- Browser error inspection reported no uncaught application errors during these
  checks. An incorrectly quoted inspection command failed outside the app and
  was corrected; it did not affect application state.
- Production build and TypeScript compilation passed; `git diff --check` passed.

## Limits and next review

Native media import, recognition, account changes, and filesystem export were
not run. Privacy/sharing is not implemented. Advanced tool interiors are the
existing Classic screens and still need the next simplification pass. See
[feature coverage and boundaries](docs/project-workspace.md).
