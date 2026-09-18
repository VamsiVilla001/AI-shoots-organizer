/**
 * Auto team-up: import an event's roster so naming a face also files that
 * player under their team.
 *
 * The file is read and shown before anything is saved — a roster with three
 * unreadable rows is worth seeing rather than silently half-importing.
 * Rosters are workspace-wide, because naming happens in Review, Albums and
 * Groups, which belong to a shoot rather than to a project.
 */

import { useRef, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { RosterPreview } from '@skwad/shared-types'
import * as api from '../api'
import { hasLocalFileDialogs, pickFiles } from '../pickers'
import { Icon } from './Icon'
import { useUi } from '../store'

export function RosterImport({ compact = false }: { compact?: boolean }) {
  const queryClient = useQueryClient()
  const pushNotice = useUi((state) => state.pushNotice)
  const [preview, setPreview] = useState<RosterPreview | null>(null)
  // A roster is a file the person has in Downloads, not one on a share the
  // server can see — so against a server the browser reads it and posts the
  // text, while the desktop hands the backend a path as before.
  const upload = useRef<HTMLInputElement>(null)

  const summary = useQuery({ queryKey: ['rosterSummary'], queryFn: api.rosterSummary })

  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ['rosterSummary'] })
    // Every open suggestion list re-reads the roster it just changed.
    void queryClient.invalidateQueries({ queryKey: ['rosterSearch'] })
  }

  const choose = useMutation({
    mutationFn: async () => {
      if (!hasLocalFileDialogs()) {
        upload.current?.click()
        return null
      }
      const picked = await pickFiles({
        multiple: false,
        title: 'Choose a roster file',
        filters: [{ name: 'Roster', extensions: ['csv', 'json', 'txt'] }],
      })
      if (!picked?.[0]) return null
      return api.previewRosterFile(picked[0])
    },
    onSuccess: (result) => {
      if (result) setPreview(result)
    },
    onError: (error) => pushNotice({ level: 'error', message: error instanceof Error ? error.message : String(error) }),
  })

  const chooseUploaded = useMutation({
    mutationFn: async (file: File) => api.previewRosterText(file.name, await file.text()),
    onSuccess: (result) => setPreview(result),
    onError: (error) => pushNotice({ level: 'error', message: error instanceof Error ? error.message : String(error) }),
  })

  const confirm = useMutation({
    mutationFn: () => api.importRoster(preview!.source, preview!.entries),
    onSuccess: (saved) => {
      setPreview(null)
      refresh()
      pushNotice({ level: 'success', message: `${saved.entries} players across ${saved.teams.length} teams are ready.` })
    },
    onError: (error) => pushNotice({ level: 'error', message: error instanceof Error ? error.message : String(error) }),
  })

  const remove = useMutation({
    mutationFn: (source: string) => api.clearRoster(source),
    onSuccess: () => {
      refresh()
      pushNotice({ level: 'success', message: 'Roster removed.' })
    },
    onError: (error) => pushNotice({ level: 'error', message: error instanceof Error ? error.message : String(error) }),
  })

  const loaded = summary.data

  return <div className={compact ? 'roster-import compact' : 'roster-import'}>
    {!compact && <h2><Icon name="players" />Auto team-up</h2>}
    <p className="hint">
      Import the event roster — team, player and in-game name. When somebody names a face, SKWAD matches what
      they type against it and adds that player's media to their team's group automatically.
    </p>

    {loaded && loaded.entries > 0 && <div className="roster-loaded">
      <div className="roster-counts">
        <strong>{loaded.entries}</strong> players · <strong>{loaded.teams.length}</strong> teams
      </div>
      <ul className="roster-sources">
        {loaded.sources.map((source) => <li key={source.source}>
          <span className="mono">{source.source}</span>
          <span className="hint">{source.entries} players</span>
          <button type="button" disabled={remove.isPending} onClick={() => {
            if (window.confirm(`Remove the roster from ${source.source}? Players already named keep their teams.`)) {
              remove.mutate(source.source)
            }
          }}><Icon name="remove" />Remove</button>
        </li>)}
      </ul>
      <div className="roster-teams">{loaded.teams.map((team) => <span key={team} className="roster-team">{team}</span>)}</div>
    </div>}

    {preview && <div className="roster-preview">
      <h3>{preview.source}</h3>
      <p className="hint">
        {preview.entries.length} players across {preview.teams.length} teams. Importing replaces anything previously
        read from this file.
      </p>
      <div className="roster-teams">{preview.teams.map((team) => <span key={team} className="roster-team">{team}</span>)}</div>
      {preview.problems.length > 0 && <details className="roster-problems">
        <summary>{preview.problems.length} row{preview.problems.length === 1 ? '' : 's'} could not be read</summary>
        <ul>{preview.problems.slice(0, 20).map((problem) => <li key={problem}>{problem}</li>)}</ul>
        {preview.problems.length > 20 && <p className="hint">…and {preview.problems.length - 20} more.</p>}
      </details>}
      <div className="actions">
        <button type="button" className="primary" disabled={confirm.isPending} onClick={() => confirm.mutate()}>
          <Icon name="save" />{confirm.isPending ? 'Importing…' : `Import ${preview.entries.length} players`}
        </button>
        <button type="button" onClick={() => setPreview(null)}>Cancel</button>
      </div>
    </div>}

    {!preview && <div className="actions">
      <button type="button" disabled={choose.isPending || chooseUploaded.isPending} onClick={() => choose.mutate()}>
        <Icon name="folder" />{choose.isPending || chooseUploaded.isPending ? 'Reading…' : loaded && loaded.entries > 0 ? 'Import another roster' : 'Choose a roster file'}
      </button>
      <input
        ref={upload}
        type="file"
        accept=".csv,.json,.txt,text/csv,application/json,text/plain"
        style={{ display: 'none' }}
        onChange={(event) => {
          const file = event.target.files?.[0]
          event.target.value = ''
          if (file) chooseUploaded.mutate(file)
        }}
      />
    </div>}

    <details className="roster-format">
      <summary>What the file should contain</summary>
      <p className="hint">A CSV with a header row, or a JSON file. Column order does not matter.</p>
      <pre className="mono">{`team,player,ign
iQOO Soul,Naresh Nallamothu,iQOOS8ULNaresh
iQOO Soul,Jonathan Amaral,iQOOS8ULJonathan
GodLike Esports,Jelly Kumar,GodLikeJelly`}</pre>
      <p className="hint">
        An optional <span className="mono">role</span> column marks coaches and staff. Rows without a team are
        reported rather than imported.
      </p>
    </details>
  </div>
}
