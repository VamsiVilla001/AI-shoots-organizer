/**
 * The name field used wherever a face is identified.
 *
 * Typing any part of a name searches the imported team rosters, so "naresh"
 * offers "iQOOS8ULNaresh · iQOO Soul" and picking it tells SKWAD which team the
 * player belongs to. Spaces, dots and case are ignored by the search, because
 * the same player is written a dozen ways.
 *
 * A native `<datalist>` cannot do this: its matching differs between the
 * Windows and macOS webviews, and it can only show a flat string, so there is
 * nowhere to put the team. Hence a real combobox, built to the ARIA pattern.
 */

import { useEffect, useId, useRef, useState, type KeyboardEvent } from 'react'
import { useQuery } from '@tanstack/react-query'
import type { RosterEntry } from '@skwad/shared-types'
import * as api from '../api'
import { Icon } from './Icon'

export interface PlayerNameFieldProps {
  value: string
  onChange: (value: string) => void
  /** Called when a roster suggestion is chosen, so callers can show the team. */
  onPick?: (entry: RosterEntry) => void
  /** Names already in the library, offered alongside the roster. */
  knownNames?: string[]
  placeholder?: string
  disabled?: boolean
  required?: boolean
  autoFocus?: boolean
  id?: string
  className?: string
  onEnter?: () => void
}

interface Suggestion {
  /** What goes into the field when this row is chosen. */
  value: string
  team: string | null
  detail: string | null
  entry: RosterEntry | null
}

export function PlayerNameField({
  value,
  onChange,
  onPick,
  knownNames = [],
  placeholder = 'Type a name — existing or new',
  disabled,
  required,
  autoFocus,
  id,
  className,
  onEnter,
}: PlayerNameFieldProps) {
  const listId = useId()
  const [open, setOpen] = useState(false)
  const [active, setActive] = useState(0)
  const wrapper = useRef<HTMLDivElement>(null)

  // The roster lives in SQLite; searching it per keystroke is a cheap indexed
  // read, and React Query keeps repeat queries for the same prefix cached.
  const roster = useQuery({
    queryKey: ['rosterSearch', value.trim()],
    queryFn: () => api.searchRoster(value.trim(), 8),
    enabled: !disabled,
    staleTime: 30_000,
  })

  const typed = value.trim().toLowerCase()
  const rosterSuggestions: Suggestion[] = (roster.data ?? []).map((entry) => ({
    value: entry.ign,
    team: entry.team,
    detail: entry.playerName && entry.playerName.toLowerCase() !== entry.ign.toLowerCase() ? entry.playerName : null,
    entry,
  }))
  // A name already in the library is worth offering too, so a reviewer does not
  // create "Naresh" beside an existing "naresh".
  const knownSuggestions: Suggestion[] = knownNames
    .filter((name) => typed !== '' && name.toLowerCase().includes(typed))
    .filter((name) => !rosterSuggestions.some((item) => item.value.toLowerCase() === name.toLowerCase()))
    .slice(0, 5)
    .map((name) => ({ value: name, team: null, detail: 'Already in your library', entry: null }))

  const suggestions = [...rosterSuggestions, ...knownSuggestions]
  const visible = open && suggestions.length > 0

  useEffect(() => setActive(0), [value])

  // Clicking anywhere else closes the list without choosing anything.
  useEffect(() => {
    if (!visible) return
    const away = (event: MouseEvent) => {
      if (!wrapper.current?.contains(event.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', away)
    return () => document.removeEventListener('mousedown', away)
  }, [visible])

  const choose = (suggestion: Suggestion) => {
    onChange(suggestion.value)
    setOpen(false)
    if (suggestion.entry) onPick?.(suggestion.entry)
  }

  const keyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'ArrowDown' && suggestions.length > 0) {
      event.preventDefault()
      setOpen(true)
      setActive((current) => (current + 1) % suggestions.length)
      return
    }
    if (event.key === 'ArrowUp' && suggestions.length > 0) {
      event.preventDefault()
      setActive((current) => (current - 1 + suggestions.length) % suggestions.length)
      return
    }
    if (event.key === 'Escape' && visible) {
      event.preventDefault()
      setOpen(false)
      return
    }
    if (event.key === 'Enter') {
      if (visible) {
        // Enter takes the highlighted suggestion rather than submitting a
        // half-typed name that would create a second person.
        event.preventDefault()
        choose(suggestions[active])
        return
      }
      onEnter?.()
    }
  }

  return (
    <div className={className ? `player-name-field ${className}` : 'player-name-field'} ref={wrapper}>
      <input
        id={id}
        type="text"
        role="combobox"
        aria-expanded={visible}
        aria-controls={listId}
        aria-autocomplete="list"
        aria-activedescendant={visible ? `${listId}-${active}` : undefined}
        autoComplete="off"
        value={value}
        placeholder={placeholder}
        disabled={disabled}
        required={required}
        autoFocus={autoFocus}
        onChange={(event) => {
          onChange(event.target.value)
          setOpen(true)
        }}
        onFocus={() => setOpen(true)}
        onKeyDown={keyDown}
      />
      {visible && (
        <ul className="player-name-list" id={listId} role="listbox">
          {suggestions.map((suggestion, index) => (
            <li
              key={`${suggestion.value}-${index}`}
              id={`${listId}-${index}`}
              role="option"
              aria-selected={index === active}
              className={index === active ? 'active' : undefined}
              onMouseEnter={() => setActive(index)}
              // mousedown, not click: the input's blur would close the list first.
              onMouseDown={(event) => {
                event.preventDefault()
                choose(suggestion)
              }}
            >
              <span className="player-name-main">
                {suggestion.entry && <Icon name="players" />}
                <span>{suggestion.value}</span>
              </span>
              <span className="player-name-meta">
                {suggestion.team && <span className="player-name-team">{suggestion.team}</span>}
                {suggestion.detail && <span>{suggestion.detail}</span>}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
