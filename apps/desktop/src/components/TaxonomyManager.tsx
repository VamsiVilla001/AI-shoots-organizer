/**
 * The taxonomy itself: every tag and the values it has been given.
 *
 * A studio maintains this list the way it maintains a roster — in a
 * spreadsheet somebody already has — so the manager imports CSV or JSON,
 * exports the current list back out, and lets tags and values be added,
 * renamed and removed in place. Removing a value here also detaches it from
 * everything it was on, which the confirmation says.
 */

import { useRef, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { TagSummary, TaxonomyPreview } from '@skwad/shared-types'
import * as api from '../api'
import { useUi } from '../store'
import { TAG_KEYS } from './TagPicker'

export function TaxonomyImportButton({ className }: { className?: string }) {
  const upload = useRef<HTMLInputElement>(null)
  const [preview, setPreview] = useState<{ source: string; text: string; parsed: TaxonomyPreview } | null>(null)
  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)

  const read = useMutation({
    mutationFn: async (file: File) => {
      const text = await file.text()
      return { source: file.name, text, parsed: await api.previewTaxonomyText(file.name, text) }
    },
    onSuccess: setPreview,
    onError: (error) => pushNotice({ level: 'error', message: error instanceof Error ? error.message : String(error) }),
  })
  const confirm = useMutation({
    mutationFn: () => api.importTaxonomyText(preview!.source, preview!.text),
    onSuccess: (summary) => {
      setPreview(null)
      void queryClient.invalidateQueries({ queryKey: TAG_KEYS.tags })
      void queryClient.invalidateQueries({ queryKey: ['tagSuggest'] })
      pushNotice({
        level: 'success',
        message: `Imported ${summary.valuesSeen} value${summary.valuesSeen === 1 ? '' : 's'} across ${summary.tagsSeen} tag${summary.tagsSeen === 1 ? '' : 's'} (${summary.tagsCreated} new tag${summary.tagsCreated === 1 ? '' : 's'}, ${summary.valuesCreated} new value${summary.valuesCreated === 1 ? '' : 's'}).`,
      })
    },
    onError: (error) => pushNotice({ level: 'error', message: error instanceof Error ? error.message : String(error) }),
  })

  return (
    <>
      <button type="button" className={className} disabled={read.isPending} onClick={() => upload.current?.click()}>
        {read.isPending ? 'Reading…' : 'Import tags'}
      </button>
      <input
        ref={upload}
        type="file"
        accept=".csv,.json,.txt,text/csv,application/json"
        style={{ display: 'none' }}
        onChange={(event) => {
          const file = event.target.files?.[0]
          event.target.value = ''
          if (file) read.mutate(file)
        }}
      />
      {preview && (
        <div className="modal-backdrop taxonomy-preview-backdrop" onClick={() => setPreview(null)}>
          <div className="modal" role="dialog" aria-label="Import tags" onClick={(event) => event.stopPropagation()}>
            <h3>Import tags from {preview.source}</h3>
            <p className="hint">
              {preview.parsed.entries.length} tag{preview.parsed.entries.length === 1 ? '' : 's'},{' '}
              {preview.parsed.entries.reduce((n, e) => n + e.values.length, 0)} values. Existing tags and values are
              kept; nothing is removed by an import.
            </p>
            <div className="taxonomy-preview">
              {preview.parsed.entries.map((entry) => (
                <div key={entry.name} className="taxonomy-preview-row">
                  <strong>{entry.name}</strong>
                  <span>{entry.values.length ? entry.values.join(' · ') : <em>no values</em>}</span>
                </div>
              ))}
            </div>
            {preview.parsed.problems.length > 0 && (
              <div className="hint taxonomy-problems">
                {preview.parsed.problems.slice(0, 8).map((problem) => (
                  <div key={problem}>{problem}</div>
                ))}
                {preview.parsed.problems.length > 8 && <div>…and {preview.parsed.problems.length - 8} more</div>}
              </div>
            )}
            <div className="db-setup-actions">
              <button type="button" className="ghost" onClick={() => setPreview(null)}>Cancel</button>
              <button
                type="button"
                className="primary"
                disabled={confirm.isPending || preview.parsed.entries.length === 0}
                onClick={() => confirm.mutate()}
              >
                {confirm.isPending ? 'Importing…' : 'Import'}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  )
}

function download(name: string, text: string, type: string) {
  const url = URL.createObjectURL(new Blob([text], { type }))
  const link = document.createElement('a')
  link.href = url
  link.download = name
  document.body.appendChild(link)
  link.click()
  link.remove()
  setTimeout(() => URL.revokeObjectURL(url), 1000)
}

export function TaxonomyManager() {
  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)
  const tags = useQuery({ queryKey: TAG_KEYS.tags, queryFn: api.listTags })
  const [newName, setNewName] = useState('')
  const [newValue, setNewValue] = useState('')
  const [filter, setFilter] = useState('')

  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: TAG_KEYS.tags })
    void queryClient.invalidateQueries({ queryKey: ['assetTags'] })
    void queryClient.invalidateQueries({ queryKey: ['tagSuggest'] })
  }
  const fail = (error: unknown) => pushNotice({ level: 'error', message: error instanceof Error ? error.message : String(error) })
  const save = useMutation({
    mutationFn: ({ name, values }: { name: string; values: string[] }) => api.saveTag(name, values),
    onSuccess: refresh,
    onError: fail,
  })
  const rename = useMutation({
    mutationFn: ({ tagId, name }: { tagId: number; name: string }) => api.renameTag(tagId, name),
    onSuccess: refresh,
    onError: fail,
  })
  const removeTag = useMutation({ mutationFn: api.deleteTag, onSuccess: refresh, onError: fail })
  const removeValue = useMutation({ mutationFn: api.deleteTagValue, onSuccess: refresh, onError: fail })
  const exportAs = useMutation({
    mutationFn: async (format: 'csv' | 'json') => ({ format, text: await api.exportTaxonomy(format) }),
    onSuccess: ({ format, text }) => {
      download(`skwad-tags.${format}`, text, format === 'json' ? 'application/json' : 'text/csv')
      navigator.clipboard?.writeText(text).catch(() => {})
      pushNotice({ level: 'success', message: `Exported as ${format.toUpperCase()} — downloaded, and copied to the clipboard.` })
    },
    onError: fail,
  })

  const query = filter.trim().toLowerCase()
  const visible = (tags.data ?? []).filter(
    (tag) =>
      !query ||
      tag.name.toLowerCase().includes(query) ||
      tag.values.some((value) => value.value.toLowerCase().includes(query)),
  )

  return (
    <div className="taxonomy-manager">
      <div className="pw-toolbar">
        <label className="pw-search">
          <span className="sr-only">Search tags</span>
          <input type="search" placeholder="Search tags and values…" value={filter} onChange={(event) => setFilter(event.target.value)} />
        </label>
        <span>{tags.data?.length ?? 0} tags</span>
        <TaxonomyImportButton />
        <button type="button" disabled={exportAs.isPending} onClick={() => exportAs.mutate('csv')}>Export CSV</button>
        <button type="button" disabled={exportAs.isPending} onClick={() => exportAs.mutate('json')}>Export JSON</button>
      </div>
      <p className="pw-help">
        Each tag is a name with the values it has been given. Values you record here, or while tagging, are
        suggested everywhere a tag is typed. A file to import is CSV (<code>tag,value</code>, or several values
        separated by semicolons) or JSON (<code>[{'{'}"name": "Team", "values": ["…"]{'}'}]</code>).
      </p>

      <form
        className="taxonomy-new"
        onSubmit={(event) => {
          event.preventDefault()
          if (!newName.trim()) return
          save.mutate({ name: newName.trim(), values: newValue.trim() ? [newValue.trim()] : [] })
          setNewName('')
          setNewValue('')
        }}
      >
        <input value={newName} placeholder="New tag name" onChange={(event) => setNewName(event.target.value)} spellCheck={false} />
        <input value={newValue} placeholder="First value (optional)" onChange={(event) => setNewValue(event.target.value)} spellCheck={false} />
        <button type="submit" className="primary" disabled={save.isPending || !newName.trim()}>Add tag</button>
      </form>

      {tags.isPending && <p className="pw-loading">Loading tags…</p>}
      {tags.isError && <p className="pw-error">Tags could not be loaded.</p>}
      {tags.data?.length === 0 && <div className="pw-empty"><h2>No tags yet</h2><p>Import a list, or add a tag above.</p></div>}
      <div className="taxonomy-list">
        {visible.map((tag) => (
          <TagRow
            key={tag.id}
            tag={tag}
            onRename={(name) => rename.mutate({ tagId: tag.id, name })}
            onAddValue={(value) => save.mutate({ name: tag.name, values: [value] })}
            onRemoveValue={(valueId, value, uses) => {
              if (uses === 0 || window.confirm(`Remove “${value}” from ${tag.name}? It is on ${uses} item${uses === 1 ? '' : 's'}, which will lose it.`))
                removeValue.mutate(valueId)
            }}
            onRemove={() => {
              const uses = tag.values.reduce((n, v) => n + v.uses, 0)
              if (window.confirm(`Delete the tag “${tag.name}” and its ${tag.values.length} value${tag.values.length === 1 ? '' : 's'}?${uses ? ` ${uses} item${uses === 1 ? '' : 's'} will lose it.` : ''}`))
                removeTag.mutate(tag.id)
            }}
          />
        ))}
      </div>
    </div>
  )
}

function TagRow({
  tag,
  onRename,
  onAddValue,
  onRemoveValue,
  onRemove,
}: {
  tag: TagSummary
  onRename: (name: string) => void
  onAddValue: (value: string) => void
  onRemoveValue: (valueId: number, value: string, uses: number) => void
  onRemove: () => void
}) {
  const [renaming, setRenaming] = useState(false)
  const [name, setName] = useState(tag.name)
  const [value, setValue] = useState('')
  const submitValue = () => {
    if (!value.trim()) return
    onAddValue(value.trim())
    setValue('')
  }
  return (
    <div className="taxonomy-row">
      <div className="taxonomy-row-head">
        {renaming ? (
          <form
            onSubmit={(event) => {
              event.preventDefault()
              if (name.trim() && name.trim() !== tag.name) onRename(name.trim())
              setRenaming(false)
            }}
          >
            <input value={name} autoFocus onChange={(event) => setName(event.target.value)} onBlur={() => setRenaming(false)} spellCheck={false} />
          </form>
        ) : (
          <strong onDoubleClick={() => setRenaming(true)} title="Double-click to rename">{tag.name}</strong>
        )}
        <span className="hint">{tag.values.length} value{tag.values.length === 1 ? '' : 's'}</span>
        <button type="button" className="small" onClick={() => setRenaming(true)}>Rename</button>
        <button type="button" className="small danger" onClick={onRemove}>Delete</button>
      </div>
      <div className="tag-chips">
        {tag.values.map((item) => (
          <span key={item.id} className="tag-chip" title={`${item.uses} use${item.uses === 1 ? '' : 's'}`}>
            {item.value}
            {item.uses > 0 && <small>{item.uses}</small>}
            <button type="button" aria-label={`Remove ${item.value}`} onClick={() => onRemoveValue(item.id, item.value, item.uses)}>×</button>
          </span>
        ))}
        <input className="tag-inline-add" value={value} placeholder="Add a value…" spellCheck={false} onChange={(event) => setValue(event.target.value)} onKeyDown={(event) => { if (event.key === 'Enter') { event.preventDefault(); submitValue() } }} />
        {value.trim() && <button type="button" className="small" onClick={submitValue}>+ Add</button>}
      </div>
    </div>
  )
}
