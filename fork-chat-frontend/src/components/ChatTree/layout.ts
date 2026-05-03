/**
 * Tree layout engine for the conversation tree visualization.
 *
 * Converts the flat turn list (with `parent_turn_id` references) into a tree
 * structure and computes 2D positions for each node using the `d3-flextree`
 * layout algorithm. The resulting positions are used to place React Flow nodes
 * on the canvas.
 */

import type { Edge, Node } from '@xyflow/react';
import { flextree } from 'd3-flextree';

/** Internal tree node used by the d3-flextree layout algorithm. */
type TreeNode = {
  id: string;
  /** [width, height] of the node's visual representation in pixels. */
  size: [number, number];
  children: TreeNode[];
};

/**
 * Converts a flat list of React Flow nodes and edges into a tree structure
 * suitable for d3-flextree.
 *
 * This function:
 * 1. Builds an adjacency list (`graph`: source -> targets) from the edges.
 * 2. Computes indegrees to find the root node (the one with indegree 0).
 * 3. Performs a DFS from the root to construct the nested `TreeNode` structure,
 *    attaching each node's measured size from `sizeMap`.
 *
 * @param sizeMap - Map from node id to [width, height] (measured DOM sizes)
 * @param nodes - Flat list of React Flow nodes
 * @param edges - Flat list of React Flow edges (source -> target)
 * @returns The root TreeNode with nested children
 */
function generateTree(
  sizeMap: Map<string, [number, number]>,
  nodes: Node[],
  edges: Edge[],
): TreeNode {
  // Build adjacency list: for each edge, record source -> target.
  const graph: Map<string, string[]> = new Map();
  // Track indegrees to identify the root (indegree == 0).
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

  // Find the root node: the only node with indegree 0.
  // In a valid conversation tree, there should be exactly one root.
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

  // DFS to build the nested tree structure from the adjacency list.
  const dfs = (node: TreeNode) => {
    const children = graph.get(node.id);
    if (!children) {
      return;
    }
    // Map each child id to a TreeNode with its measured size.
    node.children = children.map((child) => {
      return {
        id: child,
        size: sizeMap.get(child)!,
        children: [],
      };
    });
    // Recurse into children.
    for (const child of node.children) {
      dfs(child);
    }
  };
  dfs(root);

  return root;
}

/**
 * Computes 2D positions for each node in the conversation tree using the
 * d3-flextree layout algorithm.
 *
 * d3-flextree is a variant of the Reingold-Tilford tree layout that supports
 * variable node sizes (each turn card can have a different width/height based
 * on content). The algorithm:
 *
 * 1. Arranges siblings horizontally with configurable spacing.
 * 2. Centers parents over their children.
 * 3. Produces `(x, y)` coordinates where x is the horizontal offset from the
 *    tree's center and y is the depth (top-to-bottom).
 *
 * @param sizeMap - Map from node id to [width, height] (measured DOM sizes)
 * @param nodes - Flat list of React Flow nodes
 * @param edges - Flat list of React Flow edges
 * @returns Map from node id to [x, y] position for React Flow node placement
 */
export function layout(
  sizeMap: Map<string, [number, number]>,
  nodes: Node[],
  edges: Edge[],
): Map<string, [number, number]> {
  // Convert the flat graph into a nested tree for d3-flextree.
  const treeData = generateTree(sizeMap, nodes, edges);

  // Configure the flextree layout:
  // - `nodeSize`: returns [width + 40, height + 60] for each node. The extra
  //   padding (40px horizontal, 60px vertical) provides spacing between nodes
  //   so they don't overlap visually.
  // - `spacing`: additional gap between sibling nodes (50px). This stacks on
  //   top of the nodeSize padding, so total horizontal gap between two adjacent
  //   siblings is ~90px (40 + 50).
  const layout = flextree<TreeNode>({
    nodeSize: (node) => {
      const [width, height] = sizeMap.get(node.data.id)!;
      return [width + 40, height + 60];
    },
    spacing: 50,
  });

  // Build the hierarchy and compute positions.
  const tree = layout.hierarchy(treeData);
  layout(tree);

  // TODO: remove debug logging before production
  console.log(tree);

  // Extract positions from the layout result into a flat map.
  const ret: Map<string, [number, number]> = new Map();
  tree.each((node) => {
    ret.set(node.data.id!, [node.x, node.y]);
  });

  return ret;
}
