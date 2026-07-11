import dagre from '@dagrejs/dagre'
import {
  Position,
  useVueFlow,
  type GraphNode,
  type Edge as FlowEdge,
  type Node as FlowNode,
} from '@vue-flow/core'
import { ref } from 'vue'
// 自动引入vueflow样式（如果未配置自动引入，在main.ts全局import '@vue-flow/core/dist/style.css'即可）

/**
 * Composable to run the layout algorithm on the graph.
 * It uses the `dagre` library to calculate the layout of the nodes and edges.
 * 为使用者：本函数自动根据方向优化vue-flow节点布局，并兼容TS类型提示
 */
export function useLayout() {
  const { findNode } = useVueFlow()
  const graph = ref<dagre.graphlib.Graph>(new dagre.graphlib.Graph())
  // 上一次的布局方向（LR, TB等）用于对比或恢复
  const previousDirection = ref<'LR' | 'RL' | 'TB' | 'BT'>('LR')

  /**
   * 自动布局
   * @param nodes FlowNode[] 节点数组
   * @param edges FlowEdge[] 边数组
   * @param direction 'LR' | 'RL' | 'TB' | 'BT' 方向（左右/上下/……）
   */
  function layout(
    nodes: FlowNode[],
    edges: FlowEdge[],
    direction: 'LR' | 'RL' | 'TB' | 'BT' = 'LR',
  ): FlowNode[] {
    // 创建新的Graph，防止残留节点影响布局
    const dagreGraph = new dagre.graphlib.Graph()
    graph.value = dagreGraph

    dagreGraph.setDefaultEdgeLabel(() => ({}))
    dagreGraph.setGraph({ rankdir: direction })
    previousDirection.value = direction

    const isHorizontal = direction === 'LR' || direction === 'RL'

    // 设置节点尺寸，用于算法自动排布（未找到节点则使用默认尺寸）
    for (const node of nodes) {
      // findNode可能返回undefined
      const graphNode = findNode(node.id) as GraphNode | undefined
      const width = graphNode?.dimensions?.width || 150
      const height = graphNode?.dimensions?.height || 50
      dagreGraph.setNode(node.id, { width, height })
    }

    // 设置边
    for (const edge of edges) {
      if (edge.source && edge.target) {
        dagreGraph.setEdge(edge.source, edge.target)
      }
    }

    dagre.layout(dagreGraph)

    // 重新计算后的节点位置信息赋值
    return nodes.map((node) => {
      const nodeWithPosition = dagreGraph.node(node.id)
      // 提示：nodeWithPosition 可能为undefined，保险起见给默认值
      const x = nodeWithPosition?.x ?? 0
      const y = nodeWithPosition?.y ?? 0

      return {
        ...node,
        targetPosition: isHorizontal ? Position.Left : Position.Top,
        sourcePosition: isHorizontal ? Position.Right : Position.Bottom,
        position: { x, y },
      }
    })
  }

  return { graph, layout, previousDirection }
}
