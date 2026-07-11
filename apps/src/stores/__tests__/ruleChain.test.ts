import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useRuleChainStore } from '../ruleChain'

describe('stores/ruleChain.ts', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.spyOn(console, 'log').mockImplementation(() => {})
  })

  it('adds, updates and deletes rule chains while keeping current selection in sync', () => {
    const store = useRuleChainStore()

    const created = store.addRuleChain({
      name: 'Primary flow',
      description: 'main dispatch chain',
      priority: 10,
      enabled: true,
      cooldown_ms: 3000,
    })

    expect(store.ruleChains).toHaveLength(1)
    expect(store.getRuleChain(created.id)).toMatchObject({
      id: created.id,
      name: 'Primary flow',
      cooldown_ms: 3000,
    })

    store.setCurrentRuleChain(created)
    store.updateRuleChain(created.id, {
      name: 'Updated flow',
      priority: 20,
    })

    expect(store.ruleChains[0]).toMatchObject({
      id: created.id,
      name: 'Updated flow',
      priority: 20,
    })
    expect(store.hasUnsavedChanges).toBe(true)

    store.deleteRuleChain(created.id)

    expect(store.ruleChains).toHaveLength(0)
    expect(store.currentRuleChain).toBeNull()
  })

  it('toggles fullscreen and left panel state independently', () => {
    const store = useRuleChainStore()

    expect(store.isFullscreen).toBe(false)
    expect(store.isLeftPanelCollapsed).toBe(false)

    store.toggleFullscreen()
    store.toggleLeftPanel()

    expect(store.isFullscreen).toBe(true)
    expect(store.isLeftPanelCollapsed).toBe(true)
  })

  it('saves graph changes and keeps a monitor snapshot copy', () => {
    const store = useRuleChainStore()

    const nodes = [
      { id: 'n1', type: 'start', position: { x: 10, y: 20 }, data: { id: 'n1', label: 'N1' } },
    ] as any[]
    const edges = [{ id: 'e1', source: 'n1', target: 'n2' }] as any[]

    store.hasUnsavedChanges = true
    store.saveChanges(nodes as any, edges as any)

    expect(store.hasUnsavedChanges).toBe(false)
    expect(store.nodes).toEqual(nodes)
    expect(store.edges).toEqual(edges)
    expect(store.monitorNodes).toEqual(nodes)
    expect(store.monitorEdges).toEqual(edges)

    expect(store.monitorNodes).not.toBe(store.nodes)
    expect(store.monitorEdges).not.toBe(store.edges)
  })

  it('creates and restores monitor snapshots from the current graph', () => {
    const store = useRuleChainStore()

    store.nodes = [
      { id: 'start', type: 'start', position: { x: 0, y: 0 }, data: { id: 'start' } },
      { id: 'action', type: 'action', position: { x: 100, y: 0 }, data: { id: 'action' } },
    ] as any
    store.edges = [{ id: 'edge-1', source: 'start', target: 'action' }] as any

    store.createMonitorSnapshot()

    store.nodes = [{ id: 'mutated', type: 'action', position: { x: 1, y: 1 }, data: {} }] as any
    store.edges = [] as any
    store.restoreMonitorSnapshot()

    expect(store.nodes).toEqual([
      { id: 'start', type: 'start', position: { x: 0, y: 0 }, data: { id: 'start' } },
      { id: 'action', type: 'action', position: { x: 100, y: 0 }, data: { id: 'action' } },
    ])
    expect(store.edges).toEqual([{ id: 'edge-1', source: 'start', target: 'action' }])
  })

  it('initializes the default graph and exports the current rule chain payload', () => {
    const store = useRuleChainStore()

    store.initDefaultGraph()

    expect(store.nodes).toHaveLength(2)
    expect(store.nodes.map((node) => node.id)).toEqual(['start', 'end'])
    expect(store.edges).toEqual([])
    expect(store.hasUnsavedChanges).toBe(false)

    store.currentRuleChain = {
      id: 'chain-1',
      name: 'Rule Chain A',
      description: 'sync battery dispatch',
      priority: 50,
      enabled: false,
      cooldown_ms: 9000,
    }

    const exported = store.exportRuleChain(
      [
        {
          id: 'start',
          type: 'start',
          position: { x: 10, y: 20 },
          data: { id: 'start', label: 'START' },
        },
      ] as any,
      [
        {
          id: 'edge-1',
          source: 'start',
          target: 'end',
          sourceHandle: 'default',
          targetHandle: 'default',
        },
      ] as any,
    )

    expect(exported).toEqual({
      cooldown_ms: 9000,
      description: 'sync battery dispatch',
      enabled: true,
      flow_json: {
        edges: [
          {
            id: 'edge-1',
            source: 'start',
            target: 'end',
            sourceHandle: 'default',
            targetHandle: 'default',
          },
        ],
        nodes: [
          {
            id: 'start',
            type: 'start',
            position: { x: 10, y: 20 },
            data: { id: 'start', label: 'START' },
          },
        ],
      },
      format: 'vue-flow',
      id: 'chain-1',
      name: 'Rule Chain A',
      priority: 50,
    })
  })

  it('infers animated edges from active source and target nodes', () => {
    const store = useRuleChainStore()

    store.edges = [
      { id: 'edge-1', source: 'start', target: 'mid' },
      { id: 'edge-2', source: 'mid', target: 'end' },
      { id: 'edge-3', source: 'isolated', target: 'end' },
    ] as any

    expect(store.inferAnimatedEdges(['start', 'mid', 'end'])).toEqual(['edge-1', 'edge-2'])
    expect(store.inferAnimatedEdges(new Set(['mid', 'end']))).toEqual(['edge-2'])
  })

  it('clears all editor state and monitor state', () => {
    const store = useRuleChainStore()

    store.ruleChains = [
      {
        id: 'chain-1',
        name: 'Rule Chain A',
        description: 'desc',
        priority: 1,
        enabled: true,
        cooldown_ms: 1000,
      },
    ]
    store.nodes = [{ id: 'n1', type: 'start', position: { x: 0, y: 0 }, data: {} }] as any
    store.edges = [{ id: 'e1', source: 'n1', target: 'n2' }] as any
    store.currentRuleChain = store.ruleChains[0]
    store.hasUnsavedChanges = true
    store.updateMonitorNodes([{ id: 'snapshot-node' }] as any)
    store.updateMonitorEdges([{ id: 'snapshot-edge' }] as any)

    store.clearAll()

    expect(store.ruleChains).toEqual([])
    expect(store.nodes).toEqual([])
    expect(store.edges).toEqual([])
    expect(store.currentRuleChain).toBeNull()
    expect(store.hasUnsavedChanges).toBe(false)
    expect(store.monitorNodes).toEqual([{ id: 'snapshot-node' }])
    expect(store.monitorEdges).toEqual([{ id: 'snapshot-edge' }])
  })
})
