import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useParams } from '@tanstack/react-router';
import { ReactFlowProvider } from '@xyflow/react';
import { useCallback, useEffect, useState } from 'react';
import { toast } from 'sonner';
import { api } from '../api';
import { ChatTree, MessageInput, TurnDetailModal } from '../components';
import { useChatStore } from '../store';

export function ChatPage() {
  const { sessionId } = useParams({ from: '/sessions/$sessionId' });
  const queryClient = useQueryClient();
  const { selectedTurnId, setSelectedTurn } = useChatStore();
  const [modalTurnId, setModalTurnId] = useState<string | null>(null);

  const {
    data: treeData,
    isLoading: treeLoading,
    error: treeError,
  } = useQuery({
    queryKey: ['tree', sessionId],
    queryFn: () => api.turns.tree(sessionId),
    retry: false,
  });

  const sendMutation = useMutation({
    mutationFn: (data: {
      text: string;
      model: string;
      parentId: string | null;
    }) =>
      api.turns.create(sessionId, {
        user_text: data.text,
        parent_turn_id: data.parentId ?? undefined,
        provider: 'openai',
        model: data.model,
      }),
    onSuccess: (result) => {
      queryClient.invalidateQueries({ queryKey: ['tree', sessionId] });
      queryClient.invalidateQueries({ queryKey: ['sessions'] });
      setSelectedTurn(result.turn.id);
      setModalTurnId(null);
    },
    onError: (error) => {
      toast.error(
        error instanceof Error ? error.message : 'Failed to send message',
      );
    },
  });

  const retryMutation = useMutation({
    mutationFn: (data: { turnId: string; model: string }) =>
      api.turns.retry(sessionId, data.turnId, {
        provider: 'openai',
        model: data.model,
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['tree', sessionId] });
    },
    onError: (error) => {
      toast.error(error instanceof Error ? error.message : 'Retry failed');
    },
  });

  const turns = (treeData?.turns ?? []).filter(
    (t) => !(t.status === 'failed' && t.retry_turn_id != null),
  );
  const modalTurn = modalTurnId
    ? (turns.find((t) => t.id === modalTurnId) ?? null)
    : null;

  useEffect(() => {
    setSelectedTurn(null);
    setModalTurnId(null);
  }, [sessionId, setSelectedTurn]);

  const handleSelectTurn = useCallback(
    (id: string) => {
      setSelectedTurn(id);
      setModalTurnId(id);
    },
    [setSelectedTurn],
  );

  const handleSend = (text: string, model: string, parentId: string | null) => {
    sendMutation.mutate({ text, model, parentId });
  };

  const handleRetry = (turnId: string, model: string) => {
    retryMutation.mutate({ turnId, model });
  };

  return (
    <div className="h-full flex">
      <div className="flex-1">
        {treeError ? (
          <div className="flex items-center justify-center h-full text-muted-foreground">
            Session not found
          </div>
        ) : treeLoading ? (
          <div className="flex items-center justify-center h-full text-muted-foreground">
            Loading...
          </div>
        ) : turns.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-full gap-4">
            <p className="text-muted-foreground">
              Start a conversation by sending a message
            </p>
            <div className="w-full max-w-md">
              <MessageInput
                parentTurn={null}
                onSend={handleSend}
                disabled={sendMutation.isPending}
              />
            </div>
            {sendMutation.isPending && (
              <p className="text-warning text-sm">Waiting for AI...</p>
            )}
          </div>
        ) : (
          <ReactFlowProvider>
            <ChatTree
              turns={turns}
              selectedTurnId={selectedTurnId}
              onSelectTurn={handleSelectTurn}
            />
          </ReactFlowProvider>
        )}
      </div>

      <TurnDetailModal
        turn={modalTurn}
        open={modalTurnId !== null}
        onOpenChange={(open) => {
          if (!open) setModalTurnId(null);
        }}
        onSend={handleSend}
        onRetry={handleRetry}
        isSending={sendMutation.isPending || retryMutation.isPending}
      />
    </div>
  );
}
