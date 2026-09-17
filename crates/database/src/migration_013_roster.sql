-- Team rosters imported from a CSV or JSON file.
--
-- A roster is the event's own record of who plays for whom: the in-game name
-- on the jersey, the person behind it, and the team. SKWAD consults it while a
-- reviewer names a face, so naming one player is enough to file their media
-- under the right team.
--
-- Rosters are workspace-wide: naming happens in Review, Albums and Groups,
-- which belong to a shoot rather than to a project, so a roster imported for
-- one event stays available everywhere.

CREATE TABLE roster_entries (
    id          INTEGER PRIMARY KEY,
    -- The in-game name. Unique across the workspace: it is what a reviewer
    -- types and what identifies a player on the broadcast.
    ign         TEXT NOT NULL,
    -- The person behind the IGN. Optional; some rosters only carry IGNs.
    player_name TEXT NOT NULL DEFAULT '',
    team        TEXT NOT NULL,
    -- player, coach, analyst, staff, substitute… Anything not a player can be
    -- excluded from team collections without being lost.
    role        TEXT NOT NULL DEFAULT 'player',
    -- Lowercased letters and digits of the IGN, player name and team joined
    -- together, so "S8UL Naresh", "s8ulnaresh" and "naresh" all match the same
    -- row without the caller worrying about spaces, dots or case.
    search_key  TEXT NOT NULL,
    -- The file this row was imported from, so one roster can be replaced
    -- without disturbing another.
    source      TEXT NOT NULL DEFAULT '',
    imported_at TEXT NOT NULL
);

CREATE UNIQUE INDEX idx_roster_entries_ign ON roster_entries (ign COLLATE NOCASE);
CREATE INDEX idx_roster_entries_team ON roster_entries (team COLLATE NOCASE);
CREATE INDEX idx_roster_entries_search ON roster_entries (search_key);
