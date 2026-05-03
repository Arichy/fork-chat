import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useNavigate, useParams, useSearch } from '@tanstack/react-router';
import { ReactFlowProvider } from '@xyflow/react';
import { useCallback, useEffect, useState } from 'react';
import { toast } from 'sonner';
import { api } from '../api';
import { isStreamingTurnStatus, TURN_STATUS } from '../api/turnStream';
import { ChatTree, MessageInput, TurnDetailModal } from '../components';
import { useTurnStream } from '../hooks/useTurnStream';
import { useChatStore } from '../store';

export function ChatPage() {
  const { sessionId } = useParams({ from: '/sessions/$sessionId' });
  // Modal open-state is stored in the URL's `?turnId=...` search param so that
  // a page refresh (or shared link) restores the same open modal. Presence of
  // a `turnId` means the modal is open; absence means it's closed.
  const { turnId: modalTurnId } = useSearch({
    from: '/sessions/$sessionId',
  });
  const navigate = useNavigate({ from: '/sessions/$sessionId' });
  const queryClient = useQueryClient();
  const { selectedTurnId, setSelectedTurn } = useChatStore();
  const [pendingFailedTurn, setPendingFailedTurn] = useState<{
    text: string;
    provider: string;
    model: string;
    parentId: string | null;
  } | null>(null);

  // Single helper that writes modal state to the URL. Using `replace: true`
  // avoids piling up a history entry for every open/close, while still letting
  // the browser back button step out of the session if the user wants.
  const setModalTurn = useCallback(
    (turnId: string | null) => {
      navigate({
        // `undefined` removes the key from the resulting URL.
        search: (prev) => ({ ...prev, turnId: turnId ?? undefined }),
        replace: true,
      });
    },
    [navigate],
  );

  const {
    data: sessionData,
    isLoading: sessionLoading,
    error: sessionError,
  } = useQuery({
    queryKey: ['session', sessionId],
    queryFn: () => api.sessions.get(sessionId),
    retry: false,
  });
  const protocol = sessionData?.session.protocol;

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
      provider: string;
      model: string;
      parentId: string | null;
    }) =>
      api.turns.create(sessionId, {
        user_text: data.text,
        parent_turn_id: data.parentId ?? undefined,
        provider: data.provider,
        model: data.model,
      }),
    onSuccess: (result) => {
      queryClient.invalidateQueries({ queryKey: ['tree', sessionId] });
      queryClient.invalidateQueries({ queryKey: ['sessions'] });
      setPendingFailedTurn(null);
      setSelectedTurn(result.turn.id);
      // Open the modal for streaming turns so the user sees progress;
      // for already-completed turns we don't auto-open.
      setModalTurn(
        isStreamingTurnStatus(result.turn.status) ? result.turn.id : null,
      );
    },
    onError: (error, variables) => {
      queryClient.invalidateQueries({ queryKey: ['tree', sessionId] });
      queryClient.invalidateQueries({ queryKey: ['sessions'] });
      setPendingFailedTurn({
        text: variables.text,
        provider: variables.provider,
        model: variables.model,
        parentId: variables.parentId,
      });
      toast.error(
        error instanceof Error ? error.message : 'Failed to send message',
      );
    },
  });

  const retryMutation = useMutation({
    mutationFn: (data: { turnId: string; provider: string; model: string }) =>
      api.turns.retry(sessionId, data.turnId, {
        provider: data.provider,
        model: data.model,
      }),
    onSuccess: (result) => {
      queryClient.invalidateQueries({ queryKey: ['tree', sessionId] });
      setSelectedTurn(result.turn.id);
      // Same rationale as sendMutation: only auto-open while the retry is
      // still streaming.
      setModalTurn(
        isStreamingTurnStatus(result.turn.status) ? result.turn.id : null,
      );
    },
    onError: (error) => {
      toast.error(error instanceof Error ? error.message : 'Retry failed');
    },
  });

  const approveMutation = useMutation({
    mutationFn: (data: {
      turnId: string;
      pendingCallId: string;
      decision: 'allow' | 'allow_always' | 'deny';
    }) =>
      api.turns.approve(sessionId, data.turnId, {
        decisions: [
          {
            pending_call_id: data.pendingCallId,
            decision: data.decision,
          },
        ],
      }),
    onSuccess: (result) => {
      queryClient.invalidateQueries({ queryKey: ['tree', sessionId] });
      setSelectedTurn(result.turn.id);
      // Keep the modal open on the same turn so the user sees the tool result
      // land after approval.
      setModalTurn(result.turn.id);
    },
    onError: (error) => {
      toast.error(error instanceof Error ? error.message : 'Approve failed');
    },
  });

  const cancelMutation = useMutation({
    mutationFn: (data: { turnId: string }) =>
      api.turns.cancel(sessionId, data.turnId),
    onSuccess: (result) => {
      queryClient.invalidateQueries({ queryKey: ['tree', sessionId] });
      setSelectedTurn(result.turn.id);
      // Keep the modal open on the cancelled turn so the user can retry it.
      setModalTurn(result.turn.id);
    },
    onError: (error) => {
      toast.error(error instanceof Error ? error.message : 'Cancel failed');
    },
  });

  const turns = (treeData?.turns ?? []).filter(
    (t) => !(t.status === TURN_STATUS.FAILED && t.retry_turn_id != null),
  );
  const modalTurn = modalTurnId
    ? (turns.find((t) => t.id === modalTurnId) ?? null)
    : null;
  const modalTurnStatus = modalTurn?.status ?? null;

  useTurnStream({
    sessionId,
    turnId: modalTurnId ?? null,
    turnStatus: modalTurnStatus,
    queryClient,
  });

  useEffect(() => {
    if (!pendingFailedTurn) return;

    const candidate = turns
      .filter(
        (turn) =>
          turn.status === TURN_STATUS.FAILED &&
          turn.retry_turn_id == null &&
          turn.user_text === pendingFailedTurn.text &&
          turn.provider === pendingFailedTurn.provider &&
          turn.model === pendingFailedTurn.model &&
          turn.parent_turn_id === pendingFailedTurn.parentId,
      )
      .sort(
        (a, b) =>
          new Date(b.created_at).getTime() - new Date(a.created_at).getTime(),
      )[0];

    if (!candidate) return;

    setSelectedTurn(candidate.id);
    // Open the modal on the freshly-observed failed turn so the user can
    // retry immediately. Driven through the URL so a refresh preserves it.
    setModalTurn(candidate.id);
    setPendingFailedTurn(null);
  }, [pendingFailedTurn, setSelectedTurn, setModalTurn, turns]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: sessionId is the reset trigger; its value is not needed inside the effect body.
  useEffect(() => {
    // Reset in-memory state on session switch. We intentionally don't clear
    // the URL's `turnId` here: navigating between sessions produces a new URL
    // without that param, and if a user navigates to a session URL that *does*
    // carry `?turnId=...` (e.g. a shared link) we want to honor it.
    setSelectedTurn(null);
    setPendingFailedTurn(null);
  }, [sessionId, setSelectedTurn]);

  const handleSelectTurn = useCallback(
    (id: string) => {
      setSelectedTurn(id);
      setModalTurn(id);
    },
    [setSelectedTurn, setModalTurn],
  );

  const handleSend = (
    text: string,
    provider: string,
    model: string,
    parentId: string | null,
  ) => {
    sendMutation.mutate({ text, provider, model, parentId });
  };

  const handleRetry = (turnId: string, provider: string, model: string) => {
    retryMutation.mutate({ turnId, provider, model });
  };

  const handleApprove = (
    turnId: string,
    pendingCallId: string,
    decision: 'allow' | 'allow_always' | 'deny',
  ) => {
    approveMutation.mutate({ turnId, pendingCallId, decision });
  };

  const handleCancel = (turnId: string) => {
    cancelMutation.mutate({ turnId });
  };

  return (
    <div className="h-full flex">
      <div className="flex-1">
        {sessionError || treeError ? (
          <div className="flex items-center justify-center h-full text-muted-foreground">
            Session not found
          </div>
        ) : sessionLoading || treeLoading || !protocol ? (
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
                protocol={protocol}
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

      {protocol && (
        <TurnDetailModal
          turn={modalTurn}
          protocol={protocol}
          // Single source of truth: the URL param drives whether the modal
          // is open. Closing the modal clears the param; opening it sets it.
          open={modalTurnId != null}
          onOpenChange={(open) => {
            if (!open) setModalTurn(null);
          }}
          onSend={handleSend}
          onRetry={handleRetry}
          onApprove={handleApprove}
          onCancel={handleCancel}
          isSending={
            sendMutation.isPending ||
            retryMutation.isPending ||
            approveMutation.isPending ||
            cancelMutation.isPending
          }
        />
      )}
    </div>
  );
}
