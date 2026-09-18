/**
 * The worker roster: every machine enrolled to analyse for this library,
 * what it advertises and what it is doing. Administrators can revoke one;
 * the token stops working at that machine's next claim.
 */

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { MachineRosterEntry } from '@skwad/shared-types'
import * as api from '../api'
import { useUi } from '../store'

function ago(iso: string | null): string {
  if (!iso) return 'never'
  const seconds = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000)
  if (seconds < 90) return 'just now'
  if (seconds < 3600) return `${Math.round(seconds / 60)} min ago`
  if (seconds < 86_400) return `${Math.round(seconds / 3600)} h ago`
  return `${Math.round(seconds / 86_400)} d ago`
}

export function MachinesCard({ isAdmin }: { isAdmin: boolean }) {
  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)
  const machines = useQuery({ queryKey: ['machines'], queryFn: api.listMachines, refetchInterval: 10_000 })
  const revoke = useMutation({
    mutationFn: (machine: MachineRosterEntry) => api.revokeMachine(machine.id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['machines'] }),
    onError: (e) => pushNotice({ level: 'error', message: String((e as Error).message ?? e) }),
  })

  return (
    <div className="card">
      <h2>Worker machines</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Machines enrolled to analyse for this library, and what they are doing. A client enrols itself from
        its own Settings screen, signed in as an administrator.
      </p>
      {machines.isPending && <div className="hint">Loading…</div>}
      {machines.isError && <div className="hint">Could not read the roster.</div>}
      {machines.data?.length === 0 && <div className="hint">No machines enrolled. Every analysis runs here.</div>}
      {machines.data?.map((machine) => (
        <div key={machine.id} className={`machine-row${machine.revokedAt ? ' revoked' : ''}`}>
          <div className="machine-main">
            <strong>{machine.name}</strong>
            <span className="hint mono">{machine.id.slice(0, 8)}</span>
            {machine.revokedAt && <span className="hint">revoked</span>}
          </div>
          <div className="hint">
            {machine.capabilities.gpu ?? 'unknown accelerator'} · v{machine.capabilities.appVersion || '?'} ·{' '}
            {machine.capabilities.aiWorkers || 0} slots · seen {ago(machine.lastSeen)}
          </div>
          <div className="hint">
            {machine.running} running · {machine.completed} done · {machine.failed} failed
          </div>
          {isAdmin && !machine.revokedAt && (
            <button
              className="small danger"
              disabled={revoke.isPending}
              onClick={() => {
                if (window.confirm(`Revoke ${machine.name}? It stops receiving work at its next claim.`)) revoke.mutate(machine)
              }}
            >
              Revoke
            </button>
          )}
        </div>
      ))}
    </div>
  )
}
