# Esports AI Media Organiser
## Architecture & Development Plan

**Target Platforms:** Windows 10/11 and macOS on Apple Silicon  
**Primary Use Case:** Automatically organise esports media shoots by identifying players in photos and videos and generating player-wise albums.

---

# 1. Product Goal

The application is designed to reduce the manual effort required after esports photo and video shoots.

Instead of manually checking hundreds or thousands of images and separating them player-by-player, the application will:

1. Import a shoot folder.
2. Detect faces in photos and videos.
3. Generate facial embeddings.
4. Group similar faces automatically.
5. Allow the user to name a player once.
6. Remember that player for future shoots.
7. Generate player-wise AI albums.
8. Allow review and correction.
9. Export the original media into organised folders.

The application must remain focused on **media sorting for esports production**.

It is not intended to become a general Google Photos replacement.

---

# 2. Core Workflow

```text
Import Shoot
    ↓
Scan Media
    ↓
Extract Metadata
    ↓
Detect Faces
    ↓
Generate Face Embeddings
    ↓
Compare With Known Players
    ↓
Cluster Unknown Faces
    ↓
Generate AI Albums
    ↓
User Review / Rename / Merge
    ↓
Export Sorted Media
```

Example:

```text
BGMS_Player_Shoot/
│
├── Jonathan/
│   ├── Photos/
│   └── Videos/
│
├── Mavi/
│   ├── Photos/
│   └── Videos/
│
├── Jelly/
│   ├── Photos/
│   └── Videos/
│
└── Unidentified/
```

---

# 3. Main Application Modules

## 3.1 Shoots

A **Shoot** is the main working session.

Example:

```text
Shoot Name: BGMS Finals Player Shoot
Date: 09 Aug 2026
Source: D:\BGMS_Final_Shoot
```

Each shoot stores:

- Shoot ID
- Shoot name
- Date
- Source folder
- Number of photos
- Number of videos
- Processing status
- Detected people
- Generated albums
- Export status

The source files must never be modified.

---

## 3.2 Media Scanner

The Media Scanner indexes supported files from the selected shoot folder.

### Photo formats

Initial support:

- JPG
- JPEG
- PNG
- WEBP
- HEIC

Later support:

- TIFF
- RAW formats such as CR2, CR3, NEF, ARW

### Video formats

Initial support:

- MP4
- MOV
- MKV
- AVI
- WEBM

The scanner stores:

- File path
- File name
- Media type
- Width
- Height
- Duration
- File size
- Creation date
- Camera metadata when available
- Thumbnail location
- Processing state

---

# 4. Facial Recognition Pipeline

The facial recognition system should operate locally.

No Google Photos facial-recognition API is required.

## Processing Pipeline

```text
Media File
    ↓
Face Detection
    ↓
Face Alignment
    ↓
Face Crop
    ↓
Embedding Model
    ↓
Face Vector
    ↓
Similarity Search
```

A face embedding is a numerical representation of a face.

Example:

```text
Jonathan
↓
[0.012, -0.311, 0.882, ...]
```

Two embeddings representing the same person should have a high similarity score.

---

# 5. Face Detection

The detection model locates every visible face in an image.

Each detected face should store:

```text
Face ID
Media ID
Bounding Box
Confidence
Embedding
Person ID
Recognition Confidence
```

Example:

```text
Image_00231.jpg

Face 1 → Jonathan
Face 2 → Mavi
Face 3 → Unknown
```

One media file can therefore belong to multiple generated player albums.

---

# 6. Player Recognition

The application maintains a reusable local **Player Face Library**.

Example:

```text
Jonathan
├── Player ID: P001
├── Team: Optional
├── Face Samples: 86
└── Embeddings
```

When a new shoot is processed:

```text
Detected Face
      ↓
Compare Against Player Library
      ↓
Similarity Above Threshold?
      ↓
YES → Suggest Existing Player
NO  → Add To Unknown Cluster
```

Example:

```text
Detected Player

Jonathan
Confidence: 98.4%

[Accept] [Wrong Person] [Unknown]
```

A user correction should improve future recognition.

---

# 7. Unknown Face Clustering

Faces that do not match known players should automatically be grouped.

Example:

```text
Unknown Person 1
164 media

Unknown Person 2
128 media

Unknown Person 3
97 media
```

The user can then rename:

```text
Unknown Person 1
        ↓
Jonathan
```

After confirmation:

```text
Cluster
   ↓
Player Profile
   ↓
Add Face Samples
   ↓
Available For Future Shoots
```

---

# 8. AI-Generated Albums

Albums are automatically generated based on recognition results.

## Required Album Types

### Player Albums

```text
Jonathan — 164 media
Mavi — 128 media
Jelly — 97 media
```

### Multi-Player Albums

Example:

```text
Jonathan + Mavi — 35 media
Jonathan + Jelly — 18 media
```

### Unidentified

All media containing faces that still require identification.

### Photos

Player-specific photos only.

### Videos

Player-specific videos only.

### Team Albums

Optional.

If team metadata exists:

```text
Gods Reign
├── Jonathan
├── Player 2
└── Player 3
```

The first version should prioritise player-wise albums.

---

# 9. Video Recognition

Videos should not be analysed frame-by-frame because this would waste processing time.

Use FFmpeg to:

1. Read the video.
2. Detect scene changes.
3. Sample frames periodically.
4. Run facial recognition on selected frames.
5. Track recognised players through nearby frames.

Example:

```text
Tournament_Final.mp4

Jonathan
00:01:14
00:03:28
00:15:42

Mavi
00:04:11
00:09:37
```

The UI should allow clicking a detection timestamp and opening the video at that position.

---

# 10. Review Workspace

AI results should never be treated as permanently correct without allowing review.

The Review screen should support:

- Accept recognition
- Reject recognition
- Rename person
- Merge two people
- Split incorrect cluster
- Mark unknown
- Remove false face detection
- Select multiple images
- Bulk assign selected images to a player

Example:

```text
Possible Jonathan

[Image] [Image] [Image] [Image]
[Image] [Image] [Image] [Image]

Confidence Range: 86–99%

[Confirm All]
[Review Individually]
```

---

# 11. Export System

The Export module copies or links the original media into organised folders.

Default structure:

```text
Export/
│
├── Jonathan/
│   ├── Photos/
│   └── Videos/
│
├── Mavi/
│   ├── Photos/
│   └── Videos/
│
└── Unidentified/
```

Export options should include:

- Copy files
- Preserve original filenames
- Preserve metadata
- Player-wise folders
- Photos / Videos subfolders
- Export selected players only

The source media must remain untouched.

---

# 12. Recommended Technology Stack

## Desktop Framework

### Tauri 2

Recommended because it provides:

- Windows support
- macOS support
- Apple Silicon support
- Small application size
- Rust backend
- Web-based frontend
- Native filesystem access
- Good performance

---

## Frontend

### React + TypeScript

Used for:

- Media browser
- Albums
- Player manager
- Review screen
- Settings
- Export workflow

Recommended supporting tools:

```text
React
TypeScript
Vite
TanStack Query
Zustand
```

---

## Backend

### Rust

Rust handles:

- File scanning
- Media indexing
- Thumbnail jobs
- Database operations
- AI inference coordination
- FFmpeg integration
- Export operations
- Background workers

---

# 13. AI Runtime

## ONNX Runtime

Use ONNX Runtime as the common inference layer.

Architecture:

```text
                  ONNX Runtime
                       │
         ┌─────────────┴─────────────┐
         │                           │
      Windows                  Apple Silicon
         │                           │
 DirectML / WinML                 CoreML
         │                           │
        GPU                 GPU / Neural Engine
```

This allows the same AI model format to be used across platforms.

---

# 14. AI Models

The architecture should keep models replaceable.

Recommended pipeline:

```text
Face Detector
     +
Face Recognition / Embedding Model
```

Potential model families can include:

- RetinaFace-style face detection
- SCRFD-style face detection
- ArcFace-compatible recognition models

Models should be exported to ONNX wherever possible.

The application should not hard-code itself to one model.

Create an abstraction such as:

```text
FaceDetector
FaceEmbedder
FaceMatcher
FaceClusterer
```

This allows upgrading models later without rewriting the application.

---

# 15. Similarity Search

Each detected face produces an embedding vector.

For small player libraries, embeddings can initially be stored directly in SQLite.

For larger databases, add vector indexing.

Potential options:

- HNSW
- FAISS-equivalent Rust libraries
- SQLite vector extension

Initial version can use cosine similarity.

Example logic:

```text
Face Embedding
      ↓
Compare Known Embeddings
      ↓
Highest Similarity
      ↓
Above Recognition Threshold?
```

Recognition thresholds must remain configurable.

---

# 16. Database

Use SQLite for the local database.

Suggested schema:

```text
shoots
------
id
name
source_path
created_at
status

media
-----
id
shoot_id
path
filename
media_type
width
height
duration
created_at
thumbnail_path
processing_status

people
------
id
name
team
created_at

faces
-----
id
media_id
person_id
embedding
bounding_box
detection_confidence
recognition_confidence

video_detections
----------------
id
media_id
person_id
timestamp
confidence

albums
------
id
shoot_id
name
album_type

album_media
-----------
album_id
media_id
```

---

# 17. Storage Layout

Application-managed data:

```text
AppData/
│
├── database/
│   └── media.db
│
├── thumbnails/
│
├── face_cache/
│
├── models/
│
└── logs/
```

Original media remains in the user's source folders.

---

# 18. Background Processing

Indexing large shoots must not freeze the UI.

Use a background worker queue.

Example:

```text
Import
  ↓
Job Queue
  ├── Metadata
  ├── Thumbnail
  ├── Face Detection
  ├── Embedding
  ├── Recognition
  ├── Clustering
  └── Album Generation
```

The UI should show progress:

```text
Processing BGMS Shoot

Photos scanned       2,431 / 2,431
Faces detected       1,892
Players recognised   1,644
Unknown faces          248

████████████████░░░ 82%
```

Processing should be resumable after application restart.

---

# 19. Performance Strategy

To keep the application usable on normal systems:

## Photos

- Generate thumbnails first.
- Run AI on resized images.
- Keep original file untouched.
- Batch AI inference where supported.

## Videos

- Use scene detection.
- Use configurable sampling intervals.
- Avoid decoding every frame.
- Cache analysed timestamps.

## AI

- Use GPU acceleration when available.
- Fall back to CPU automatically.
- Avoid loading models repeatedly.
- Run inference in dedicated worker threads.

---

# 20. Application UI

Recommended navigation:

```text
┌─────────────────────────────────────────────┐
│ Esports AI Media Organiser                 │
├───────────────┬─────────────────────────────┤
│ Shoots        │                             │
│ Players       │        Workspace            │
│ AI Albums     │                             │
│ Review        │                             │
│ Export        │                             │
│ Settings      │                             │
└───────────────┴─────────────────────────────┘
```

---

# 21. Shoots Screen

Example:

```text
Recent Shoots

BGMS Finals Shoot
2,431 Photos
128 Videos
14 Players
Completed

Valorant Player Shoot
1,204 Photos
46 Videos
8 Players
Processing 72%
```

Actions:

- New Shoot
- Resume Processing
- Open Shoot
- Export
- Delete Index

Deleting a shoot index should not delete the user's original media.

---

# 22. Players Screen

Example:

```text
Players

Jonathan
164 media
86 face samples

Mavi
128 media
64 face samples

Jelly
97 media
51 face samples
```

Player profile:

```text
Jonathan

Known Faces: 86
Shoots: 12
Media: 1,932

[Edit Name]
[Add Face Sample]
[Review Matches]
[Delete Recognition Data]
```

---

# 23. AI Albums Screen

Example:

```text
BGMS Finals

Players
────────────────────

Jonathan                 164
Mavi                     128
Jelly                     97

Multiple Players
────────────────────

Jonathan + Mavi           35
Jonathan + Jelly          18

Needs Review
────────────────────

Unknown Person 1          22
Unknown Person 2          11
```

---

# 24. Privacy

Facial recognition data should remain local by default.

Requirements:

- No face data uploaded automatically.
- No third-party cloud dependency.
- Delete player profile option.
- Delete embeddings option.
- Clear all recognition data option.
- Original media never modified.

---

# 25. Logging

Maintain lightweight application logs.

Example:

```json
{
  "timestamp": "2026-08-09T10:45:21",
  "event": "player_assignment",
  "shoot": "BGMS Finals",
  "media": "IMG_00231.JPG",
  "person": "Jonathan",
  "confidence": 0.984
}
```

Logs should cover:

- Shoot import
- Processing errors
- Player creation
- Player rename
- Cluster merge
- Manual correction
- Export

Avoid storing unnecessary biometric data in logs.

---

# 26. Suggested Project Structure

```text
esports-media-ai/
│
├── apps/
│   └── desktop/
│       ├── src/
│       └── src-tauri/
│
├── crates/
│   ├── media-core/
│   ├── face-detection/
│   ├── face-recognition/
│   ├── clustering/
│   ├── database/
│   ├── video-analysis/
│   └── export-engine/
│
├── models/
│
├── packages/
│   └── shared-types/
│
├── docs/
│
└── tests/
```

---

# 27. Processing Architecture

```text
                     React UI
                        │
                     Tauri IPC
                        │
                        ▼
                  Rust Application
                        │
          ┌─────────────┼─────────────┐
          │             │             │
          ▼             ▼             ▼
     Media Core      Database      Job Queue
          │                           │
          │              ┌────────────┼────────────┐
          │              ▼            ▼            ▼
          │        Face Detector  Embeddings    FFmpeg
          │              │            │            │
          └──────────────┴──────┬─────┴────────────┘
                                ▼
                        Recognition Engine
                                │
                       ┌────────┴────────┐
                       ▼                 ▼
                 Known Player       Unknown Cluster
                       │                 │
                       └────────┬────────┘
                                ▼
                         Album Generator
                                │
                                ▼
                           Review / Export
```

---

# 28. Development Phases

## Phase 1 — Desktop Foundation

Build:

- Tauri application
- React interface
- SQLite database
- Folder selection
- Shoot creation
- Media scanner
- Thumbnail generation
- Basic media grid

Goal:

Import and browse a large shoot reliably.

---

## Phase 2 — Photo Face Detection

Build:

- ONNX Runtime integration
- Face detector
- Face crops
- Bounding-box preview
- Store face records in SQLite

Goal:

Detect all faces inside imported photos.

---

## Phase 3 — Facial Recognition

Build:

- Face embedding model
- Similarity comparison
- Person database
- Name unknown person
- Assign faces to person
- Recognition confidence

Goal:

The application starts remembering players.

---

## Phase 4 — Face Clustering

Build:

- Unknown-face clustering
- Cluster preview
- Merge clusters
- Split incorrect cluster
- Rename cluster
- Add confirmed cluster to Player Library

Goal:

Automatically organise unknown players.

---

## Phase 5 — AI Albums

Build:

- Player-wise albums
- Multi-player albums
- Unidentified album
- Photo/video filters
- Album counts

Goal:

Generate useful production-ready media groups automatically.

---

## Phase 6 — Review System

Build:

- Confidence filtering
- Accept/reject
- Bulk confirmation
- Manual reassignment
- Merge people
- Remove wrong face

Goal:

Allow fast human verification of AI results.

---

## Phase 7 — Export

Build:

- Player-wise folders
- Copy original media
- Photos/videos subfolders
- Preserve filenames
- Export selected albums

Goal:

Provide immediately usable sorted folders for editors and designers.

---

## Phase 8 — Video Recognition

Build:

- FFmpeg integration
- Scene-change detection
- Frame sampling
- Face detection in sampled frames
- Player timestamps
- Video preview/jump-to-time

Goal:

Include video shoots in the same player-based workflow.

---

## Phase 9 — Performance Optimisation

Build:

- GPU provider selection
- CoreML support
- DirectML/WinML support
- Batch processing
- Worker pool
- Resume interrupted jobs
- Cache management

Goal:

Handle thousands of files smoothly.

---

# 29. MVP Scope

The first usable MVP should contain:

- Windows support
- Apple Silicon support
- Create Shoot
- Import folder
- JPG/PNG/HEIC support
- MP4/MOV support
- Thumbnail browser
- Face detection
- Face embeddings
- Similar-face clustering
- Name players
- Remember players
- Player-wise AI albums
- Unknown-player album
- Review results
- Export original files into player folders
- Local SQLite database
- Background processing
- No cloud requirement

---

# 30. Out of Scope

The following should intentionally NOT be part of the initial product:

- Google Photos library integration
- OneDrive integration
- iCloud Photos integration
- NAS management
- Map/location search
- General object recognition
- Natural-language media search
- Social-media publishing
- Photo editing
- Video editing
- Cloud facial recognition

This keeps the product focused on one strong use case.

---

# 31. Product Success Criteria

The application succeeds if a production team can:

1. Import a large esports shoot.
2. Allow AI to process it.
3. Identify each player only once.
4. Automatically receive player-wise albums.
5. Review uncertain matches quickly.
6. Export sorted media.

The core target is:

> **Reduce hours of manual player-image sorting into a short AI-assisted review workflow.**

---

# 32. Recommended First Build Order

```text
1. Tauri + React shell
2. SQLite schema
3. Shoot creation
4. Media scanner
5. Thumbnail system
6. Face detection
7. Face embeddings
8. Player database
9. Similarity matching
10. Unknown clustering
11. Player albums
12. Review UI
13. Export
14. Video analysis
15. GPU optimisation
```

Do not begin with advanced UI design or video recognition.

The most important technical proof should be:

```text
1000 player photos
        ↓
Detect faces
        ↓
Cluster same players correctly
        ↓
Name each cluster
        ↓
Export player folders
```

Once this works reliably, the rest of the product can be built around it.

---

# 33. Final Architecture Decision

Recommended stack:

```text
Desktop       → Tauri 2
Frontend      → React + TypeScript
Backend       → Rust
AI Runtime    → ONNX Runtime
Face Models   → ONNX detector + embedding model
Video         → FFmpeg
Database      → SQLite
Acceleration  → CoreML / DirectML or WinML
Storage       → Local-first
```

This architecture keeps the application:

- Cross-platform
- Fast
- Local
- Private
- GPU accelerated
- Easy to package
- Focused specifically on esports player media sorting

---

# 34. Manual Grouping (Editor-Named Folders)

Face recognition proposes; the editor decides. AI albums are derived state —
`albums::regenerate` drops and rebuilds them from face assignments — so they
cannot hold a human decision. Manual **groups** are that missing layer, and
they are what the export writes by default.

## The job this removes

An editor cutting one player's reel opens a raw shoot folder, works out which
files belong to whom, and copies them into per-person folders by hand. That
copying is the hours being spent. Here they name a group once in the app and
every file they file into it lands in a folder of that name on the NAS.

## Rules

- **The source folder is read-only.** A group is a set of pointers into the
  media index. Creating, emptying or deleting one changes nothing on disk.
- **The name is the folder.** A group's name becomes one folder in the export
  destination, sanitised for the strictest target filesystem
  (`sanitise_component`). A group may carry a `folder_name` override for when
  the on-disk name should differ from the label being worked with
  (`01_Jonathan` for a folder that has to sort first, say).
- **A file may belong to several groups.** A clip with two players in it is
  legitimately both players' footage, so membership is many-to-many and the
  file is copied into each folder. "Move here" exists for the other case —
  something filed under the wrong person — and takes the file out of every
  other group in the shoot.
- **Manual work outranks AI work.** Re-analysing a shoot, clearing embeddings
  or regenerating albums never touches `media_groups`.
- **Groups are per shoot.** Files from another shoot cannot be added, so an
  export can never silently mix two source folders.

## Screens

*Sort into Groups* is where a shoot opens. A fixed panel lists the groups with
their file counts and the folder each will produce, plus two standing views —
*Not sorted yet* (the backlog an editor works to zero) and *All files*. The
grid to its right is selection-first: click to pick, shift-click for a range,
drag a selection onto a group, or use the selection bar to add to an existing
group or type a new name. Thumbnails carry a chip per group they already sit
in, so a mis-file is visible without opening anything.

*Build groups from AI players* seeds one group per identified player,
pre-filled with that player's album, so the editor corrects rather than sorts
from scratch. It is re-runnable: naming more faces and running it again tops
the groups up and never undoes a manual edit. A single album can also be
promoted from the AI Albums screen.

## Export

The Export screen writes either the editor's groups (the default) or the AI
albums directly. Group mode takes a group selection; the preview lists the
exact folders before anything is written, and a `_sorting-report.txt` in the
destination records which source file went where.

```text
\\NAS\Edit\BGMS_Finals_Sorted/
│
├── _sorting-report.txt
├── Jonathan/
│   ├── Photos/
│   └── Videos/
├── Mavi_ Day 2/
│   └── Videos/
└── Team B-roll/
    └── Photos/
```

## Storage

```sql
media_groups       (id, shoot_id, name, folder_name, notes, person_id,
                    sort_order, media_count, photo_count, video_count,
                    cover_media_id, created_at, updated_at)
media_group_items  (group_id, media_id, added_at)
```

Counts are denormalised, as `albums` does it, because the sorting screen
renders them on every interaction. `UNIQUE (shoot_id, name)` with a
`NOCASE` collation stops two groups from fighting over one folder.
