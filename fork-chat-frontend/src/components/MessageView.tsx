import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { TURN_STATUS } from '../api/turnStream';
import type { Turn } from '../api/types';

interface MessageViewProps {
  turn: Turn;
}

export function MessageView({ turn }: MessageViewProps) {
  return (
    <div className="p-4 border rounded-lg bg-white">
      <div className="flex items-center gap-2 mb-2 text-xs text-gray-500">
        <span
          className={[
            'px-2 py-1 rounded',
            turn.status === TURN_STATUS.COMPLETED
              ? 'bg-green-100 text-green-800'
              : '',
            turn.status === TURN_STATUS.RUNNING
              ? 'bg-yellow-100 text-yellow-800'
              : '',
            turn.status === TURN_STATUS.FAILED ? 'bg-red-100 text-red-800' : '',
          ].join(' ')}
        >
          {turn.status}
        </span>
        <span>{turn.model}</span>
        {turn.input_tokens && (
          <span>
            • {turn.input_tokens} in / {turn.output_tokens} out
          </span>
        )}
      </div>

      {turn.user_text && (
        <div className="mb-3">
          <div className="text-xs text-gray-400 mb-1">User:</div>
          <div className="text-gray-800 markdown-content">
            <ReactMarkdown remarkPlugins={[remarkGfm]}>
              {turn.user_text}
            </ReactMarkdown>
          </div>
        </div>
      )}

      {turn.assistant_text && (
        <div>
          <div className="text-xs text-gray-400 mb-1">Assistant:</div>
          <div className="text-gray-700 markdown-content">
            <ReactMarkdown remarkPlugins={[remarkGfm]}>
              {turn.assistant_text}
            </ReactMarkdown>
          </div>
        </div>
      )}

      {turn.error && (
        <div className="mt-2 p-2 bg-red-50 text-red-600 rounded">
          Error: {JSON.stringify(turn.error)}
        </div>
      )}
    </div>
  );
}
