import {
  Background,
  Controls,
  type Edge,
  Handle,
  MarkerType,
  MiniMap,
  type Node,
  type NodeProps,
  Position,
  ReactFlow,
  useEdgesState,
  useNodesInitialized,
  useNodesState,
  useReactFlow,
} from '@xyflow/react';
import { keyBy } from 'es-toolkit/array';
import {
  type RefObject,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { TURN_STATUS } from '../../api/turnStream';
import '@xyflow/react/dist/style.css';
import type { Turn } from '../../api/types';
import { layout } from './layout';

const NODE_WIDTH = 600;

type TurnNodeData = {
  turn: Turn;
  isSelected: boolean;
  nodeRefs: RefObject<Map<string, HTMLDivElement>>;
  onSelect: (turnId: string) => void;
};

type TurnNodeType = Node<TurnNodeData>;

function TurnNode({ id, data }: NodeProps<TurnNodeType>) {
  const { turn, isSelected, onSelect, nodeRefs } = data;
  const contentRef = useRef<HTMLDivElement>(null);
  const [overflowing, setOverflowing] = useState(false);

  // biome-ignore lint/correctness/useExhaustiveDependencies: Re-measure overflow only when rendered message content changes.
  useLayoutEffect(() => {
    const el = contentRef.current;
    if (el) {
      setOverflowing(el.scrollHeight > el.clientHeight);
    }
  }, [turn.user_text, turn.assistant_text]);

  const bgClass = [
    'bg-white',
    isSelected ? 'bg-blue-50' : '',
    turn.status === TURN_STATUS.RUNNING ? 'bg-yellow-50' : '',
    turn.status === TURN_STATUS.FAILED ? 'bg-red-50' : '',
  ]
    .filter(Boolean)
    .join(' ');

  const gradientFrom = isSelected
    ? 'from-blue-50'
    : turn.status === TURN_STATUS.RUNNING
      ? 'from-yellow-50'
      : turn.status === TURN_STATUS.FAILED
        ? 'from-red-50'
        : 'from-white';

  return (
    <div
      id={id}
      ref={(ref) => {
        nodeRefs.current.set(id, ref!);
      }}
      className={[
        `px-3 py-2 rounded-lg border-2 cursor-pointer`,
        'bg-white shadow-md',
        isSelected ? 'border-blue-500 bg-blue-50' : 'border-gray-300',
        turn.status === TURN_STATUS.RUNNING
          ? 'border-yellow-500 bg-yellow-50'
          : '',
        turn.status === TURN_STATUS.FAILED ? 'border-red-500 bg-red-50' : '',
      ].join(' ')}
      style={{ width: NODE_WIDTH }}
      onClick={() => onSelect(turn.id)}
    >
      <Handle
        type="target"
        position={Position.Top}
        className="!bg-slate-400 !w-2 !h-2"
      />
      <div className="text-xs text-gray-500 mb-1">
        {turn.status} • {turn.model || 'unknown'}
      </div>
      <div
        ref={contentRef}
        className={['max-h-[200px] overflow-hidden relative', bgClass].join(
          ' ',
        )}
      >
        {turn.user_text && (
          <div className="text-sm font-medium text-gray-800 markdown-content">
            <ReactMarkdown remarkPlugins={[remarkGfm]}>
              {turn.user_text}
            </ReactMarkdown>
          </div>
        )}
        {turn.assistant_text && (
          <div className="text-sm text-gray-600 mt-1 markdown-content">
            <ReactMarkdown remarkPlugins={[remarkGfm]}>
              {turn.assistant_text}
            </ReactMarkdown>
          </div>
        )}
        {overflowing && (
          <div
            className={`absolute bottom-0 left-0 right-0 h-8 bg-gradient-to-t ${gradientFrom} to-transparent pointer-events-none`}
          />
        )}
      </div>
      <Handle
        type="source"
        position={Position.Bottom}
        className="!bg-slate-400 !w-2 !h-2"
      />
    </div>
  );
}

const nodeTypes = {
  turn: TurnNode,
};

interface ChatTreeProps {
  turns: Turn[];
  selectedTurnId: string | null;
  onSelectTurn: (turnId: string) => void;
}

export function ChatTree({
  turns,
  selectedTurnId,
  onSelectTurn,
}: ChatTreeProps) {
  const nodeRefs = useRef<Map<string, HTMLDivElement>>(null) as RefObject<
    Map<string, HTMLDivElement>
  >;

  if (nodeRefs.current === null) {
    nodeRefs.current = new Map();
  }

  const initialNodes: TurnNodeType[] = (() => {
    return turns.map((turn) => {
      return {
        id: turn.id,
        type: 'turn',
        position: {
          x: 0,
          y: 0,
        },

        data: {
          isSelected: turn.id === selectedTurnId,
          turn,
          nodeRefs,
          onSelect: onSelectTurn,
        },
      };
    });
  })();

  const initialEdges: Edge[] = turns
    .filter((turn) => turn.parent_turn_id)
    .map((turn) => ({
      id: `e-${turn.parent_turn_id}-${turn.id}`,
      source: turn.parent_turn_id!,
      target: turn.id,
      type: 'smoothstep',
      animated: turn.status === TURN_STATUS.RUNNING,
      markerEnd: { type: MarkerType.ArrowClosed, width: 20, height: 20 },
      style: { stroke: '#64748b', strokeWidth: 2 },
    }));

  const [nodes, setNodes, onNodesChange] = useNodesState(initialNodes);
  const [edges, setEdges, onEdgesChange] = useEdgesState(initialEdges);

  // biome-ignore lint/correctness/useExhaustiveDependencies: Preserve existing behavior: React Flow node/edge state is resynced only when tree data changes.
  useEffect(() => {
    setNodes((prev) => {
      const nodeHash = keyBy(prev, (node) => node.id);
      return initialNodes.map((node) => {
        const existing = nodeHash[node.id];
        if (existing) {
          return { ...existing, data: node.data };
        }
        return node;
      });
    });
    setEdges(initialEdges);
  }, [turns]);

  const nodesInitialized = useNodesInitialized();

  const { getNodes, getEdges, fitView } = useReactFlow<TurnNodeType>();
  // biome-ignore lint/correctness/useExhaustiveDependencies: Layout intentionally runs when React Flow reports measured nodes initialized; React Flow methods are stable for this effect.
  useEffect(() => {
    if (!nodesInitialized) {
      return;
    }
    const nodes = getNodes();
    const edges = getEdges();

    const sizeMap: Map<string, [number, number]> = new Map();

    for (const node of nodes) {
      sizeMap.set(node.id, [node.measured!.width!, node.measured!.height!]);
    }

    const ret = layout(sizeMap, nodes, edges);
    const layouted = nodes.map((node) => {
      let [x, y] = [0, 0];

      if (ret.has(node.id)) {
        [x, y] = ret.get(node.id)!;
      }

      return {
        ...node,
        position: {
          x: x,
          y: y,
        },
      };
    });
    setNodes(layouted);
    fitView({ minZoom: 0.5 });
  }, [nodesInitialized]);

  if (turns.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-gray-400">
        No messages yet
      </div>
    );
  }

  return (
    <ReactFlow
      nodes={nodes}
      edges={edges}
      onNodesChange={onNodesChange}
      onEdgesChange={onEdgesChange}
      nodeTypes={nodeTypes}
      fitView
      panOnScroll={true}
      zoomOnScroll={false}
      zoomOnPinch={true}
      panOnDrag={[0, 1, 2]}
      selectionOnDrag={false}
      maxZoom={1.5}
      className="bg-gray-50"
      proOptions={{
        hideAttribution: true,
      }}
    >
      <Background />
      <Controls showInteractive={false} />
      <MiniMap />
    </ReactFlow>
  );
}
