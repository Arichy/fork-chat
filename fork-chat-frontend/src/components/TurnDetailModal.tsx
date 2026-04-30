import { RefreshCw } from 'lucide-react';
import { useRef } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import type { Protocol, Turn } from '../api/types';
import { MessageInput } from './MessageInput';
import { Button } from './ui/button';
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from './ui/dialog';

interface TurnDetailModalProps {
  turn: Turn | null;
  protocol: Protocol;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSend: (
    text: string,
    provider: string,
    model: string,
    parentId: string | null,
  ) => void;
  onRetry: (turnId: string, provider: string, model: string) => void;
  isSending: boolean;
}

export function TurnDetailModal({
  turn,
  protocol,
  open,
  onOpenChange,
  onSend,
  onRetry,
  isSending,
}: TurnDetailModalProps) {
  const lastTurnRef = useRef<Turn | null>(null);
  if (turn) lastTurnRef.current = turn;
  const displayTurn = turn ?? lastTurnRef.current;

  if (!displayTurn) return null;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-4xl max-h-[85vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>
            {displayTurn.user_text?.slice(0, 80) || 'Assistant response'}
          </DialogTitle>
          <DialogClose />
        </DialogHeader>

        <div className="flex items-center gap-2 mb-4 text-xs text-gray-500">
          <span
            className={[
              'px-2 py-1 rounded',
              displayTurn.status === 'completed'
                ? 'bg-green-100 text-green-800'
                : '',
              displayTurn.status === 'running'
                ? 'bg-yellow-100 text-yellow-800'
                : '',
              displayTurn.status === 'failed' ? 'bg-red-100 text-red-800' : '',
            ].join(' ')}
          >
            {displayTurn.status}
          </span>
          <span>{displayTurn.model}</span>
          {displayTurn.input_tokens && (
            <span>
              {displayTurn.input_tokens} in / {displayTurn.output_tokens} out
            </span>
          )}
        </div>

        <div className="flex-1 space-y-4">
          {displayTurn.user_text && (
            <div>
              <div className="text-xs text-gray-400 mb-1 font-medium">User</div>
              <div className="text-gray-800 markdown-content">
                <ReactMarkdown remarkPlugins={[remarkGfm]}>
                  {displayTurn.user_text}
                </ReactMarkdown>
              </div>
            </div>
          )}

          {displayTurn.assistant_text && (
            <div>
              <div className="text-xs text-gray-400 mb-1 font-medium">
                Assistant
              </div>
              <div className="text-gray-700 markdown-content">
                <ReactMarkdown remarkPlugins={[remarkGfm]}>
                  {displayTurn.assistant_text}
                </ReactMarkdown>
              </div>
            </div>
          )}

          {displayTurn.error && (
            <div className="p-3 bg-red-50 text-red-600 rounded text-sm">
              Error: {JSON.stringify(displayTurn.error)}
            </div>
          )}
        </div>

        <div className="border-t pt-4 mt-4">
          {isSending && (
            <div className="text-center text-sm text-muted-foreground mb-2">
              Waiting for AI response...
            </div>
          )}
          {displayTurn.status === 'failed' && (
            <div className="mb-3">
              <Button
                variant="outline"
                className="w-full"
                disabled={isSending}
                onClick={() => {
                  onRetry(
                    displayTurn.id,
                    displayTurn.provider ?? '',
                    displayTurn.model ?? '',
                  );
                }}
              >
                <RefreshCw className="size-4 mr-1" />
                Retry
              </Button>
            </div>
          )}
          {displayTurn.status !== 'failed' && (
            <MessageInput
              key={displayTurn.id}
              parentTurn={displayTurn}
              protocol={protocol}
              onSend={onSend}
              disabled={isSending}
            />
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
