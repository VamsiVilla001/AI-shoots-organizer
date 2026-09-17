/**
 * The operations behind "add media to a collection" — used by the Add to
 * existing collection dialog and by pasting a Cut/Copy clipboard onto a
 * collection, so both land media the same way.
 */

import type { QueryClient } from '@tanstack/react-query'
import type { Media } from '@skwad/shared-types'
import * as api from '../api'
import { GROUP_CHANGE_KEYS, invalidateKeys } from '../queryKeys'
import type { Project, ProjectCollection } from './model'

/**
 * Files a selection into `collection`.
 *
 * A collection points at `{shootId, groupId}` sources, so each file joins the
 * source group belonging to its own shoot. A selection spanning a shoot the
 * collection has never covered grows it a new source rather than being
 * dropped. Returns how many rows were genuinely new — the backend ignores
 * files already in the group, so re-pasting is harmless.
 */
export async function addMediaToCollection(
  media: Media[],
  project: Project,
  collection: ProjectCollection,
  projects: Project[],
  save: (projects: Project[]) => void,
  client: QueryClient,
): Promise<number> {
  const sources = [...collection.sources]
  let added = 0

  for (const shootId of new Set(media.map(item => item.shootId))) {
    let source = sources.find(item => item.shootId === shootId)
    if (!source) {
      // create_group is get-or-create, so a same-named group already in this
      // media source is reused instead of duplicated.
      const group = await api.createGroup(shootId, collection.name)
      source = { shootId, groupId: group.id }
      sources.push(source)
    }
    added += await api.addMediaToGroup({
      ...source,
      mediaIds: media.filter(item => item.shootId === shootId).map(item => item.id),
      moveFiles: false,
    })
  }

  if (sources.length !== collection.sources.length) {
    const stamp = new Date().toISOString()
    save(projects.map(item => item.id === project.id
      ? { ...item, collections: item.collections.map(child => child.id === collection.id ? { ...child, sources, updatedAt: stamp } : child) }
      : item))
  }
  await invalidateKeys(client, GROUP_CHANGE_KEYS)
  return added
}

/** Takes files back out of the group they were cut from. */
export async function removeMediaFromGroup(groupId: number, media: Media[], client: QueryClient) {
  await api.removeMediaFromGroup(groupId, media.map(item => item.id))
  await invalidateKeys(client, GROUP_CHANGE_KEYS)
}
