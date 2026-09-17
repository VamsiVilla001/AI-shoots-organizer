# One library, shared across the network

SKWAD runs entirely on your own machines. A team can still work out of one
library: a single folder on a machine or NAS that stays on holds everything the
application manages, and every installation points at it.

That folder holds:

| Folder | What it is |
| --- | --- |
| `database/media.db` | Media index, faces, people, albums, projects, collections and per-user profiles |
| `auth/credentials.json` | The sign-in accounts and their roles |
| `thumbnails/`, `proxies/`, `face_cache/` | Generated previews and face crops |
| `models/` | The detection and recognition models |
| `logs/` | Application logs |

Face embeddings live in the database, so recognition learned on one workstation
applies on all of them. Your media files are **not** copied into the library —
they stay wherever they are, and every machine must be able to reach them by
the same path.

## Setting it up

1. On the machine or NAS that stays on, create a folder, e.g. `D:\skwad-library`.
2. Share it on the network with read **and** write access for the team, and
   note its UNC path, e.g. `\\STUDIO-PC\skwad-library`.
3. In SKWAD, open **Settings → Team library location**, put that path in
   **Shared library folder**, and save. SKWAD checks it can create and delete
   files there before saving.
4. Restart SKWAD when it asks. The title of the card then shows the shared
   folder as the location in use.
5. Send the same path to everyone else. Each person does steps 3 and 4 on their
   own machine — the pointer is per installation.

To move an existing library into the share, close SKWAD everywhere and copy the
contents of the old folder (the card shows its path) into the shared folder
before pointing anyone at it. Changing the location never copies or deletes
anything by itself.

`SKWAD_LIBRARY_ROOT` overrides the setting, which is how a deployment script
can configure a machine without anyone opening Settings.

## What changes on a share

- **Database journalling.** SQLite's WAL mode needs shared memory that SMB
  cannot provide, so a library on a share uses the rollback journal,
  `synchronous=FULL`, and a 30-second busy timeout. SKWAD switches
  automatically for `\\server\share` paths; tick **This folder is on the
  network** yourself when the share is reached through a mapped drive letter,
  because a drive letter is indistinguishable from a local disk.
- **Writes wait for each other.** One workstation writing blocks the others for
  the length of that write. Importing and analysing a shoot is write-heavy, so
  let one machine do the heavy processing while the rest browse and sort.
- **Previews cross the wire.** Tick **Keep thumbnails and previews on this
  machine** to keep `thumbnails/`, `proxies/` and `face_cache/` on a local disk.
  Browsing gets faster on a busy network; the cost is that each machine builds
  its own previews. The database, accounts and embeddings stay shared either
  way.
- **The network must be reliable.** A share that disappears mid-write is the
  one way to damage a SQLite library, so put the folder on a machine with a
  wired connection, and back the folder up. If the share is unreachable at
  launch, SKWAD falls back to the machine's own library rather than refusing to
  start — check the card in Settings if a workstation suddenly looks empty.

## Accounts follow the library

Because `auth/credentials.json` is inside the library, everyone signs in
against the same roster once they point at the share, and an administrator
managing users in **Settings → Users** manages them for the whole team. Before
a machine is pointed at the share, it has its own local seeded roster — see
[local authentication](local-auth.md).
