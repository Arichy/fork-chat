import type { Edge, Node } from '@xyflow/react';
import { describe, expect, it } from 'vitest';
import { layout } from './layout';

function makeNode(id: string): Node {
  return {
    id,
    position: { x: 0, y: 0 },
    data: {},
  };
}

function makeEdge(source: string, target: string): Edge {
  return { id: `${source}->${target}`, source, target };
}

describe('layout', () => {
  it('returns a single entry for a single-node tree', () => {
    const sizeMap = new Map<string, [number, number]>([['a', [200, 100]]]);
    const nodes = [makeNode('a')];
    const edges: Edge[] = [];

    const result = layout(sizeMap, nodes, edges);
    expect(result.size).toBe(1);
    expect(result.has('a')).toBe(true);
    const [, y] = result.get('a')!;
    // Root sits at the top (y = 0) in a flextree vertical layout.
    expect(y).toBe(0);
  });

  it('places every node in a linear parent->child chain', () => {
    const sizeMap = new Map<string, [number, number]>([
      ['root', [200, 100]],
      ['mid', [200, 100]],
      ['leaf', [200, 100]],
    ]);
    const nodes = [makeNode('root'), makeNode('mid'), makeNode('leaf')];
    const edges = [makeEdge('root', 'mid'), makeEdge('mid', 'leaf')];

    const result = layout(sizeMap, nodes, edges);
    expect(result.size).toBe(3);

    const rootY = result.get('root')![1];
    const midY = result.get('mid')![1];
    const leafY = result.get('leaf')![1];
    // Children should be below their parents on the Y axis.
    expect(midY).toBeGreaterThan(rootY);
    expect(leafY).toBeGreaterThan(midY);
  });

  it('positions all nodes in a branching tree', () => {
    const sizeMap = new Map<string, [number, number]>([
      ['r', [200, 100]],
      ['a', [200, 100]],
      ['b', [200, 100]],
    ]);
    const nodes = [makeNode('r'), makeNode('a'), makeNode('b')];
    const edges = [makeEdge('r', 'a'), makeEdge('r', 'b')];

    const result = layout(sizeMap, nodes, edges);
    expect(result.size).toBe(3);
    // Siblings should share a Y-coordinate and differ on X.
    const aPos = result.get('a')!;
    const bPos = result.get('b')!;
    expect(aPos[1]).toBe(bPos[1]);
    expect(aPos[0]).not.toBe(bPos[0]);
  });

  it('throws "No root found" when every node has an incoming edge (cycle)', () => {
    const sizeMap = new Map<string, [number, number]>([
      ['a', [200, 100]],
      ['b', [200, 100]],
    ]);
    const nodes = [makeNode('a'), makeNode('b')];
    const edges = [makeEdge('a', 'b'), makeEdge('b', 'a')];

    expect(() => layout(sizeMap, nodes, edges)).toThrow('No root found');
  });

  it('scales sibling spacing with node width', () => {
    const smallSizes = new Map<string, [number, number]>([
      ['r', [100, 50]],
      ['a', [100, 50]],
      ['b', [100, 50]],
    ]);
    const largeSizes = new Map<string, [number, number]>([
      ['r', [400, 50]],
      ['a', [400, 50]],
      ['b', [400, 50]],
    ]);
    const nodes = [makeNode('r'), makeNode('a'), makeNode('b')];
    const edges = [makeEdge('r', 'a'), makeEdge('r', 'b')];

    const small = layout(smallSizes, nodes, edges);
    const large = layout(largeSizes, nodes, edges);

    const smallGap = Math.abs(small.get('a')![0] - small.get('b')![0]);
    const largeGap = Math.abs(large.get('a')![0] - large.get('b')![0]);
    // Wider nodes must produce a wider sibling gap.
    expect(largeGap).toBeGreaterThan(smallGap);
  });
});
