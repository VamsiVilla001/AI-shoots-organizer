/**
 * Tags on one thing — a photo, an automatic face group, a collection.
 *
 * Shows what is attached as chips grouped by tag, and lets the person add
 * another `Tag: value` pair. Both halves suggest as you type: tag names
 * from the taxonomy, values from everything already recorded under that
 * tag (or under any tag, when none is chosen yet). A tag may carry several
 * values on the same asset, which is why adding never replaces.
 */

import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { AssetTag, TagAssetKind, TagSuggestion } from '@skwad/shared-types'
import * as api from '../api'
import { useUi } from '../store'

/** The id every tag-name `<datalist>` shares, filled once per screen. */
const TAG_NAMES_LIST = 'skwad-tag-names'

/** Query keys everything here invalidates after a change. */
export const TAG_KEYS = {
  tags: ['tags'] as const,
  asset: (kind: TagAssetKind, key: string) => ['assetTags', kind, key] as const,
  assets: (kind: TagAssetKind) => ['assetTags', kind] as const,
}

/** Renders the shared tag-name datalist. Mount once per screen that tags. */
export function TagNamesDatalist() {
  const tags = useQuery({ queryKey: TAG_KEYS.tags, queryFn: api.listTags, staleTime: 30_000 })
  return (
    <datalist id={TAG_NAMES_LIST}>
      {(tags.data ?? []).map((tag) => (
        <option key={tag.id} value={tag.name} />
      ))}
    </datalist>
  )
}

/**
 * A value box with suggestions from the taxonomy. `tag` narrows the
 * suggestions to one tag; `null` searches every tag, which is what a search
 * field wants.
 */
export function TagValueInput({
  tag,
  value,
  onChange,
  onPick,
  onSubmit,
  placeholder,
  autoFocus,
  className,
}: {
  tag: string | null
  value: string
  onChange: (value: string) => void
  /** Called with the chosen suggestion; the value is set through `onChange` first. */
  onPick?: (suggestion: TagSuggestion) => void
  onSubmit?: () => void
  placeholder?: string
  autoFocus?: boolean
  className?: string
}) {
  const [query, setQuery] = useState(value)
  const [open, setOpen] = useState(false)
  const [active, setActive] = useState(0)
  useEffect(() => {
    const timer = setTimeout(() => setQuery(value), 150)
    return () => clearTimeout(timer)
  }, [value])
  const suggestions = useQuery({
    queryKey: ['tagSuggest', tag ?? '', query],
    queryFn: () => api.suggestTagValues(tag, query, 12),
    enabled: open,
    staleTime: 10_000,
  })
  const shown = useMemo(
    () => (suggestions.data ?? []).filter((s) => s.value.toLowerCase() !== value.trim().toLowerCase() || s.tag !== tag),
    [suggestions.data, value, tag],
  )
  const pick = (suggestion: TagSuggestion) => {
    onChange(suggestion.value)
    onPick?.(suggestion)
    setOpen(false)
  }
  const onKey = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'ArrowDown' && shown.length > 0) {
      event.preventDefault()
      setOpen(true)
      setActive((current) => Math.min(current + 1, shown.length - 1))
    } else if (event.key === 'ArrowUp' && shown.length > 0) {
      event.preventDefault()
      setActive((current) => Math.max(current - 1, 0))
    } else if (event.key === 'Enter') {
      event.preventDefault()
      if (open && shown[active]) pick(shown[active])
      else onSubmit?.()
    } else if (event.key === 'Escape') {
      setOpen(false)
    }
  }
  return (
    <div className={`tag-value-input${className ? ` ${className}` : ''}`}>
      <input
        value={value}
        placeholder={placeholder ?? 'Value'}
        autoFocus={autoFocus}
        onChange={(event) => {
          onChange(event.target.value)
          setOpen(true)
          setActive(0)
        }}
        onFocus={() => setOpen(true)}
        onBlur={() => setTimeout(() => setOpen(false), 120)}
        onKeyDown={onKey}
        spellCheck={false}
      />
      {open && shown.length > 0 && (
        <ul className="tag-suggestions" role="listbox">
          {shown.map((suggestion, index) => (
            <li
              key={suggestion.valueId}
              role="option"
              aria-selected={index === active}
              className={index === active ? 'active' : undefined}
              onMouseDown={(event) => {
                event.preventDefault()
                pick(suggestion)
              }}
            >
              <span>{suggestion.value}</span>
              {tag === null && <small>{suggestion.tag}</small>}
              {suggestion.uses > 0 && <small>{suggestion.uses}×</small>}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

/** The `Tag: value` editor on its own, for callers that apply the pair themselves. */
export function TagPairEditor({
  onAdd,
  busy,
  autoFocus,
  addLabel = 'Add',
}: {
  onAdd: (tag: string, value: string) => void
  busy?: boolean
  autoFocus?: boolean
  addLabel?: string
}) {
  const [tag, setTag] = useState('')
  const [value, setValue] = useState('')
  const tagRef = useRef<HTMLInputElement>(null)
  const submit = () => {
    if (!tag.trim() || !value.trim()) return
    onAdd(tag.trim(), value.trim())
    setValue('')
  }
  return (
    <div className="tag-pair-editor" onClick={(event) => event.stopPropagation()}>
      <input
        ref={tagRef}
        list={TAG_NAMES_LIST}
        value={tag}
        placeholder="Tag"
        autoFocus={autoFocus}
        onChange={(event) => setTag(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === 'Enter') event.preventDefault()
        }}
        spellCheck={false}
      />
      <TagValueInput
        tag={tag.trim() || null}
        value={value}
        onChange={setValue}
        onPick={(suggestion) => {
          if (!tag.trim()) setTag(suggestion.tag)
        }}
        onSubmit={submit}
        placeholder="Value"
      />
      <button type="button" className="small primary" disabled={busy || !tag.trim() || !value.trim()} onClick={submit}>
        {busy ? '…' : addLabel}
      </button>
    </div>
  )
}

/** Chips for a list of assignments, grouped by tag, with optional removal. */
export function TagChips({ tags, onRemove }: { tags: AssetTag[]; onRemove?: (tag: AssetTag) => void }) {
  const grouped = useMemo(() => {
    const byTag = new Map<string, AssetTag[]>()
    for (const item of tags) byTag.set(item.tag, [...(byTag.get(item.tag) ?? []), item])
    return [...byTag.entries()]
  }, [tags])
  if (grouped.length === 0) return null
  return (
    <div className="tag-chips">
      {grouped.map(([name, values]) => (
        <span key={name} className="tag-group">
          <span className="tag-group-name">{name}</span>
          {values.map((item) => (
            <span key={item.valueId} className="tag-chip">
              {item.value}
              {onRemove && (
                <button type="button" aria-label={`Remove ${name} ${item.value}`} onClick={(event) => { event.stopPropagation(); onRemove(item) }}>
                  ×
                </button>
              )}
            </span>
          ))}
        </span>
      ))}
    </div>
  )
}

/**
 * The full picker for one asset: its chips, and an editor to add more.
 * `compact` keeps the editor behind a small button until it is wanted, for
 * cards in a grid.
 */
export function TagPicker({
  kind,
  assetKey,
  compact = false,
  label,
}: {
  kind: TagAssetKind
  assetKey: string
  compact?: boolean
  label?: string
}) {
  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)
  const [editing, setEditing] = useState(!compact)
  const tags = useQuery({ queryKey: TAG_KEYS.asset(kind, assetKey), queryFn: () => api.assetTags(kind, assetKey) })

  const settle = (next: AssetTag[]) => {
    queryClient.setQueryData(TAG_KEYS.asset(kind, assetKey), next)
    void queryClient.invalidateQueries({ queryKey: TAG_KEYS.tags })
    void queryClient.invalidateQueries({ queryKey: ['tagSuggest'] })
  }
  const fail = (error: unknown) => pushNotice({ level: 'error', message: error instanceof Error ? error.message : String(error) })
  const add = useMutation({
    mutationFn: ({ tag, value }: { tag: string; value: string }) => api.assignTag(kind, assetKey, tag, value),
    onSuccess: settle,
    onError: fail,
  })
  const remove = useMutation({
    mutationFn: (item: AssetTag) => api.unassignTag(kind, assetKey, item.valueId),
    onSuccess: settle,
    onError: fail,
  })

  const count = tags.data?.length ?? 0
  return (
    <div className={`tag-picker${compact ? ' compact' : ''}`} onClick={(event) => event.stopPropagation()}>
      {label && <span className="tag-picker-label">{label}</span>}
      <TagChips tags={tags.data ?? []} onRemove={(item) => remove.mutate(item)} />
      {editing ? (
        <TagPairEditor busy={add.isPending} autoFocus={compact} onAdd={(tag, value) => add.mutate({ tag, value })} />
      ) : (
        <button type="button" className="small tag-add" onClick={() => setEditing(true)}>
          {count === 0 ? '+ Tag' : '+ Add tag'}
        </button>
      )}
      {compact && editing && (
        <button type="button" className="small ghost tag-done" onClick={() => setEditing(false)}>
          Done
        </button>
      )}
    </div>
  )
}
