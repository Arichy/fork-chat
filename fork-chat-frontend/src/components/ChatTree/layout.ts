import type { Edge, Node } from '@xyflow/react';
import { flextree } from 'd3-flextree';

type TreeNode = {
  id: string;
  size: [number, number];
  children: TreeNode[];
};

function generateTree(
  sizeMap: Map<string, [number, number]>,
  nodes: Node[],
  edges: Edge[],
): TreeNode {
  const graph: Map<string, string[]> = new Map();
  const indegrees: Map<string, number> = new Map();
  for (const node of nodes) {
    indegrees.set(node.id, 0);
  }

  for (const edge of edges) {
    const targets = graph.get(edge.source) ?? [];
    targets.push(edge.target);
    graph.set(edge.source, targets);
    const prev = indegrees.get(edge.target) ?? 0;
    indegrees.set(edge.target, prev + 1);
  }

  let rootId: string = '';
  for (const [id, indegree] of indegrees.entries()) {
    if (indegree === 0) {
      rootId = id;
      break;
    }
  }

  if (!rootId) {
    throw 'No root found';
  }

  const root: TreeNode = {
    id: rootId,
    size: sizeMap.get(rootId)!,
    children: [],
  };

  const dfs = (node: TreeNode) => {
    const children = graph.get(node.id);
    if (!children) {
      return;
    }
    node.children = children.map((child) => {
      return {
        id: child,
        size: sizeMap.get(child)!,
        children: [],
      };
    });
    for (const child of node.children) {
      dfs(child);
    }
  };
  dfs(root);

  return root;
}

export function layout(
  sizeMap: Map<string, [number, number]>,
  nodes: Node[],
  edges: Edge[],
): Map<string, [number, number]> {
  const treeData = generateTree(sizeMap, nodes, edges);

  const layout = flextree<TreeNode>({
    nodeSize: (node) => {
      const [width, height] = sizeMap.get(node.data.id)!;
      return [width + 40, height + 60];
    },
    spacing: 50,
  });

  const tree = layout.hierarchy(treeData);

  layout(tree);

  console.log(tree);
  const ret: Map<string, [number, number]> = new Map();
  tree.each((node) => {
    ret.set(node.data.id!, [node.x, node.y]);
  });

  return ret;
}
