import { defineStore } from 'pinia'
import { ref, watch } from 'vue'
import type { RuleChain } from '@/types/ruleConfiguration'
import type { Node, Edge } from '@vue-flow/core'

export const useRuleChainStore = defineStore('ruleChain', () => {
  const ruleChains = ref<RuleChain[]>([])
  const nodes = ref<Node[]>([
    // {
    //   id: 'start',
    //   type: 'start',
    //   position: { x: 100, y: 100 },
    //   data: {
    //     id: 'start',
    //     type: 'start',
    //     label: 'START',
    //     description: 'START',
    //     config: { wires: { default: [] } },
    //   },
    //   deletable: false,
    // },
    // {
    //   id: 'end',
    //   type: 'end',
    //   position: { x: 500, y: 100 },
    //   data: {
    //     id: 'end',
    //     type: 'end',
    //     label: 'END',
    //     description: 'END',
    //     config: { wires: { default: [] } },
    //   },
    //   deletable: false,
    // },
  ])
  const edges = ref<Edge[]>([])
  const currentRuleChain = ref<RuleChain | null>(null)
  const isFullscreen = ref(false)
  const isLeftPanelCollapsed = ref(false)
  const hasUnsavedChanges = ref(false)

  // 监控模式下的节点和边副本（用于记录拖拽等操作，不影响编辑模式）
  const monitorNodes = ref<Node[]>([])
  const monitorEdges = ref<Edge[]>([])

  const addRuleChain = (ruleChainData: Omit<RuleChain, 'id' | 'createdAt' | 'updatedAt'>) => {
    const newRuleChain: RuleChain = {
      id: `chain-${Date.now()}`,
      ...ruleChainData,
    }
    ruleChains.value.push(newRuleChain)
    return newRuleChain
  }

  const updateRuleChain = (id: string, updates: Partial<Omit<RuleChain, 'id' | 'createdAt'>>) => {
    const index = ruleChains.value.findIndex((chain) => chain.id === id)
    if (index !== -1) {
      ruleChains.value[index] = {
        ...ruleChains.value[index],
        ...updates,
      }
      hasUnsavedChanges.value = true
    }
  }

  watch(hasUnsavedChanges, (newVal) => {
    console.log('hasUnsavedChanges', newVal)
  })

  const deleteRuleChain = (id: string) => {
    const index = ruleChains.value.findIndex((chain) => chain.id === id)
    if (index !== -1) {
      ruleChains.value.splice(index, 1)
      if (currentRuleChain.value?.id === id) {
        currentRuleChain.value = null
      }
    }
  }

  const getRuleChain = (id: string) => ruleChains.value.find((chain) => chain.id === id)

  const setCurrentRuleChain = (ruleChain: RuleChain) => {
    currentRuleChain.value = ruleChain
  }

  const addNodes = (node: Node[]) => {
    nodes.value.push(...node)
  }

  const addEdges = (edge: Edge[]) => {
    edges.value.push(...edge)
  }

  const toggleFullscreen = () => {
    isFullscreen.value = !isFullscreen.value
  }

  const toggleLeftPanel = () => {
    isLeftPanelCollapsed.value = !isLeftPanelCollapsed.value
  }

  const saveChanges = (newNodes: Node[], newEdges: Edge[]) => {
    hasUnsavedChanges.value = false
    nodes.value = newNodes
    edges.value = newEdges
    // 提交后同步更新监控模式副本
    monitorNodes.value = JSON.parse(JSON.stringify(newNodes))
    monitorEdges.value = JSON.parse(JSON.stringify(newEdges))
  }

  // 创建监控模式副本（从当前节点和边深拷贝）
  const createMonitorSnapshot = () => {
    monitorNodes.value = JSON.parse(JSON.stringify(nodes.value))
    monitorEdges.value = JSON.parse(JSON.stringify(edges.value))
  }

  // 恢复监控模式副本到当前节点和边
  const restoreMonitorSnapshot = () => {
    if (monitorNodes.value.length > 0 || monitorEdges.value.length > 0) {
      nodes.value = JSON.parse(JSON.stringify(monitorNodes.value))
      edges.value = JSON.parse(JSON.stringify(monitorEdges.value))
    }
  }

  // 更新监控模式下的节点和边
  const updateMonitorNodes = (newNodes: Node[]) => {
    monitorNodes.value = JSON.parse(JSON.stringify(newNodes))
  }

  const updateMonitorEdges = (newEdges: Edge[]) => {
    monitorEdges.value = JSON.parse(JSON.stringify(newEdges))
  }

  const discardChanges = () => {
    hasUnsavedChanges.value = false
  }

  // 当规则详情为空时的初始化：仅包含 START 与 END 节点，edges 为空
  const initDefaultGraph = () => {
    nodes.value = [
      {
        id: 'start',
        type: 'start',
        position: { x: 100, y: 100 },
        data: {
          id: 'start',
          type: 'start',
          label: 'START',
          description: 'START',
          config: { wires: { default: [] } },
        },
        deletable: false,
      },
      {
        id: 'end',
        type: 'end',
        position: { x: 500, y: 100 },
        data: {
          id: 'end',
          type: 'end',
          label: 'END',
          description: 'END',
          config: { wires: { default: [] } },
        },
        deletable: false,
      },
    ]
    edges.value = []
    hasUnsavedChanges.value = false
  }

  const exportRuleChain = (customNodes?: Node[], customEdges?: Edge[]) => {
    // 如果传入了自定义的 nodes 和 edges，使用它们；否则使用 store 中的数据
    const nodesToExport = customNodes || nodes.value
    const edgesToExport = customEdges || edges.value

    const flow_json = {
      edges: edgesToExport.map((edge) => ({
        id: edge.id,
        source: edge.source,
        target: edge.target,
        ...(edge.sourceHandle ? { sourceHandle: edge.sourceHandle } : {}),
        ...(edge.targetHandle ? { targetHandle: edge.targetHandle } : {}),
      })),
      nodes: nodesToExport.map((node) => {
        return {
          id: node.id,
          type: node.type,
          position: node.position,
          ...(node.data ? { data: node.data } : {}),
        }
      }),
    }
    return {
      cooldown_ms: currentRuleChain.value?.cooldown_ms || 5000,
      description: currentRuleChain.value?.description || '',
      enabled: currentRuleChain.value?.enabled || true,
      flow_json,
      format: 'vue-flow',
      id: currentRuleChain.value?.id || `chain-${Date.now()}`,
      name: currentRuleChain.value?.name || 'Untitled Rule Chain',
      priority: currentRuleChain.value?.priority || 100,
    }
  }

  const clearAll = () => {
    ruleChains.value = []
    nodes.value = []
    edges.value = []
    currentRuleChain.value = null
    hasUnsavedChanges.value = false
  }

  /**
   * 根据“亮起的节点”推断需要动画的边（一个统一样式）
   * 规则：源节点和目标节点都在激活集合中的边视为“本次分支”的动画边
   * 说明：外部只需传入激活节点列表（整条分支的节点集合），本函数返回应高亮的边ID列表
   */
  const inferAnimatedEdges = (activeNodeIds: string[] | Set<string>) => {
    const active = new Set<string>(
      Array.isArray(activeNodeIds) ? activeNodeIds : Array.from(activeNodeIds),
    )
    return edges.value
      .filter((e) => active.has(String(e.source)) && active.has(String(e.target)))
      .map((e) => String(e.id))
  }

  return {
    ruleChains,
    nodes,
    edges,
    currentRuleChain,
    isFullscreen,
    isLeftPanelCollapsed,
    hasUnsavedChanges,
    monitorNodes,
    monitorEdges,
    addRuleChain,
    updateRuleChain,
    deleteRuleChain,
    getRuleChain,
    setCurrentRuleChain,
    addNodes,
    addEdges,
    toggleFullscreen,
    toggleLeftPanel,
    saveChanges,
    discardChanges,
    initDefaultGraph,
    exportRuleChain,
    clearAll,
    inferAnimatedEdges,
    createMonitorSnapshot,
    restoreMonitorSnapshot,
    updateMonitorNodes,
    updateMonitorEdges,
  }
})
