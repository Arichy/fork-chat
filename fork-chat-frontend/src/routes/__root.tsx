import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  createRootRoute,
  Link,
  Outlet,
  useNavigate,
} from '@tanstack/react-router';
import {
  Ellipsis,
  MessageSquare,
  PanelLeftClose,
  PanelLeftOpen,
  Plus,
} from 'lucide-react';
import { useRef, useState } from 'react';
import { Toaster } from 'sonner';
import { api } from '../api';
import type { Protocol } from '../api/types';
import { Button } from '../components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../components/ui/dialog';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '../components/ui/dropdown-menu';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '../components/ui/select';
import { useChatStore } from '../store';

type SessionSummary = {
  id: string;
  title: string | null;
  created_at: string;
};

type SessionGroupLabel = 'Today' | 'Yesterday' | 'Previous 7 days' | 'Older';

const GROUP_ORDER: SessionGroupLabel[] = [
  'Today',
  'Yesterday',
  'Previous 7 days',
  'Older',
];

function startOfDay(d: Date): number {
  const copy = new Date(d);
  copy.setHours(0, 0, 0, 0);
  return copy.getTime();
}

function groupSessions(
  sessions: SessionSummary[],
): Record<SessionGroupLabel, SessionSummary[]> {
  const today = startOfDay(new Date());
  const day = 24 * 60 * 60 * 1000;
  const groups: Record<SessionGroupLabel, SessionSummary[]> = {
    Today: [],
    Yesterday: [],
    'Previous 7 days': [],
    Older: [],
  };
  for (const s of sessions) {
    const ts = startOfDay(new Date(s.created_at));
    const diff = today - ts;
    if (diff <= 0) groups.Today.push(s);
    else if (diff === day) groups.Yesterday.push(s);
    else if (diff < 7 * day) groups['Previous 7 days'].push(s);
    else groups.Older.push(s);
  }
  return groups;
}

// Compact per-row timestamp. Since rows are already grouped by date bucket,
// show time for Today/Yesterday and a short date otherwise.
function formatRowTimestamp(iso: string, group: SessionGroupLabel): string {
  const date = new Date(iso);
  if (group === 'Today' || group === 'Yesterday') {
    return date.toLocaleTimeString(undefined, {
      hour: '2-digit',
      minute: '2-digit',
    });
  }
  const sameYear = date.getFullYear() === new Date().getFullYear();
  return date.toLocaleDateString(undefined, {
    month: 'short',
    day: 'numeric',
    year: sameYear ? undefined : '2-digit',
  });
}

export const Route = createRootRoute({
  component: RootComponent,
});

function SessionSidebar() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const { setSelectedTurn } = useChatStore();
  const [collapsed, setCollapsed] = useState(false);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState('');
  const [pendingDelete, setPendingDelete] = useState<{
    id: string;
    title: string;
  } | null>(null);
  // Keep the last title displayed while the dialog animates closed,
  // so cancel/confirm doesn't show an empty title flash.
  const lastDeleteTitleRef = useRef<string>('');
  if (pendingDelete) lastDeleteTitleRef.current = pendingDelete.title;
  const deleteDialogTitle = pendingDelete?.title ?? lastDeleteTitleRef.current;

  const {
    data: sessions,
    isLoading,
    error,
  } = useQuery({
    queryKey: ['sessions'],
    queryFn: api.sessions.list,
  });

  const createMutation = useMutation({
    mutationFn: (protocol: Protocol) => api.sessions.create({ protocol }),
    onSuccess: (result) => {
      qc.invalidateQueries({ queryKey: ['sessions'] });
      navigate({
        to: '/sessions/$sessionId',
        params: { sessionId: result.session.id },
      });
    },
  });

  // Protocol the next "new session" click will use. Defaults to openai; the
  // inline Select lets users switch before clicking +.
  const [newSessionProtocol, setNewSessionProtocol] =
    useState<Protocol>('openai');

  const { data: configData } = useQuery({
    queryKey: ['config'],
    queryFn: api.config.get,
  });
  const availableProtocols: Protocol[] = configData?.protocols ?? [
    'openai',
    'anthropic',
  ];

  const deleteMutation = useMutation({
    mutationFn: (id: string) => api.sessions.delete(id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['sessions'] });
      setSelectedTurn(null);
      navigate({ to: '/' });
    },
  });

  const renameMutation = useMutation({
    mutationFn: ({ id, title }: { id: string; title: string }) =>
      api.sessions.updateTitle(id, title),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['sessions'] });
      setRenamingId(null);
    },
  });

  const handleRename = (id: string, currentTitle: string | null) => {
    setRenamingId(id);
    setRenameValue(currentTitle || '');
  };

  const handleDelete = (id: string, title: string | null) => {
    setPendingDelete({ id, title: title || 'Untitled' });
  };

  const confirmDelete = () => {
    if (pendingDelete) {
      deleteMutation.mutate(pendingDelete.id);
      setPendingDelete(null);
    }
  };

  if (collapsed) {
    return (
      <div className="h-full bg-zinc-900 text-zinc-100 flex flex-col items-center py-3 w-12 border-r border-zinc-800">
        <button
          type="button"
          onClick={() => setCollapsed(false)}
          className="p-1.5 text-zinc-400 hover:text-zinc-100 hover:bg-zinc-800 rounded-md mb-2 transition-colors cursor-pointer"
          title="Expand sidebar"
        >
          <PanelLeftOpen className="size-4" />
        </button>
        <button
          type="button"
          onClick={() => createMutation.mutate(newSessionProtocol)}
          disabled={createMutation.isPending}
          className="p-1.5 text-zinc-400 hover:text-zinc-100 hover:bg-zinc-800 rounded-md transition-colors disabled:opacity-50 cursor-pointer disabled:cursor-not-allowed"
          title={`New ${newSessionProtocol} session`}
        >
          <Plus className="size-4" />
        </button>
      </div>
    );
  }

  return (
    <div className="w-64 h-full bg-zinc-900 text-zinc-100 flex flex-col border-r border-zinc-800">
      <div className="p-3 border-b border-zinc-800 flex items-center justify-between">
        <h1 className="font-semibold tracking-tight">Fork Chat</h1>
        <div className="flex items-center gap-1">
          <Select
            value={newSessionProtocol}
            onValueChange={(v) => setNewSessionProtocol(v as Protocol)}
          >
            <SelectTrigger className="h-7 px-2 text-xs" size="sm">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {availableProtocols.map((p) => (
                <SelectItem key={p} value={p}>
                  {p}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Button
            onClick={() => createMutation.mutate(newSessionProtocol)}
            disabled={createMutation.isPending}
            size="icon-xs"
            variant="secondary"
            title={`New ${newSessionProtocol} session`}
          >
            <Plus className="size-3.5" />
          </Button>
          <button
            type="button"
            onClick={() => setCollapsed(true)}
            className="p-1 text-zinc-400 hover:text-zinc-100 hover:bg-zinc-800 rounded-md transition-colors cursor-pointer"
            title="Collapse sidebar"
          >
            <PanelLeftClose className="size-4" />
          </button>
        </div>
      </div>

      <div className="flex-1 overflow-auto px-2 py-2">
        {isLoading && (
          <div className="p-3 text-xs text-zinc-500">Loading...</div>
        )}
        {error && (
          <div className="p-3 text-xs text-red-400">Error: {error.message}</div>
        )}

        {sessions &&
          sessions.length > 0 &&
          (() => {
            const groups = groupSessions(sessions);
            return GROUP_ORDER.filter((g) => groups[g].length > 0).map(
              (group) => (
                <div key={group} className="mb-3 last:mb-0">
                  <div className="px-2 pt-1 pb-1 text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
                    {group}
                  </div>
                  <div className="flex flex-col gap-0.5">
                    {groups[group].map((session) => (
                      <div key={session.id} className="group/row relative">
                        <Link
                          to="/sessions/$sessionId"
                          params={{ sessionId: session.id }}
                          className="flex items-center gap-2 px-2.5 h-8 pr-8 rounded-md text-sm text-zinc-300 hover:bg-zinc-800/60 transition-colors"
                          activeProps={{
                            className:
                              'bg-zinc-800 text-zinc-50 hover:bg-zinc-800',
                          }}
                          onClick={() => {
                            setSelectedTurn(null);
                          }}
                        >
                          <MessageSquare className="size-3.5 shrink-0 text-zinc-500 group-hover/row:text-zinc-400" />
                          {renamingId === session.id ? (
                            <input
                              autoFocus
                              value={renameValue}
                              onChange={(e) => setRenameValue(e.target.value)}
                              onBlur={() => {
                                if (renameValue.trim()) {
                                  renameMutation.mutate({
                                    id: session.id,
                                    title: renameValue.trim(),
                                  });
                                } else {
                                  setRenamingId(null);
                                }
                              }}
                              onKeyDown={(e) => {
                                if (e.key === 'Enter') {
                                  e.preventDefault();
                                  (e.target as HTMLInputElement).blur();
                                }
                                if (e.key === 'Escape') {
                                  setRenamingId(null);
                                }
                              }}
                              onClick={(e) => e.preventDefault()}
                              className="flex-1 min-w-0 text-sm bg-zinc-900 border border-zinc-600 rounded px-1.5 py-0.5 outline-none focus:border-zinc-400"
                            />
                          ) : (
                            <>
                              <span className="flex-1 truncate">
                                {session.title || 'Untitled'}
                              </span>
                              <span
                                className="shrink-0 text-[11px] tabular-nums text-zinc-500 group-hover/row:text-zinc-400 transition-colors"
                                title={new Date(
                                  session.created_at,
                                ).toLocaleString()}
                              >
                                {formatRowTimestamp(session.created_at, group)}
                              </span>
                            </>
                          )}
                        </Link>

                        <div className="absolute right-1 top-1/2 -translate-y-1/2 opacity-0 group-hover/row:opacity-100 focus-within:opacity-100 transition-opacity">
                          <DropdownMenu>
                            <DropdownMenuTrigger>
                              <button
                                type="button"
                                onClick={(e) => e.preventDefault()}
                                className="p-1 text-zinc-400 hover:text-zinc-100 hover:bg-zinc-700 rounded-md cursor-pointer"
                              >
                                <Ellipsis className="size-3.5" />
                              </button>
                            </DropdownMenuTrigger>
                            <DropdownMenuContent align="end" className="w-32">
                              <DropdownMenuItem
                                onClick={(e) => {
                                  e.preventDefault();
                                  handleRename(session.id, session.title);
                                }}
                              >
                                Rename
                              </DropdownMenuItem>
                              <DropdownMenuItem
                                variant="destructive"
                                onClick={(e) => {
                                  e.preventDefault();
                                  handleDelete(session.id, session.title);
                                }}
                              >
                                Delete
                              </DropdownMenuItem>
                            </DropdownMenuContent>
                          </DropdownMenu>
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
              ),
            );
          })()}

        {sessions?.length === 0 && !isLoading && (
          <div className="p-4 text-zinc-500 text-center text-sm">
            No sessions yet. Click + to start one.
          </div>
        )}
      </div>

      <Dialog
        open={pendingDelete !== null}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(null);
        }}
      >
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>Delete session?</DialogTitle>
            <DialogDescription>
              &ldquo;{deleteDialogTitle}&rdquo; and all of its turns will be
              permanently removed. This action cannot be undone.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="secondary" onClick={() => setPendingDelete(null)}>
              Cancel
            </Button>
            <Button
              variant="destructive"
              onClick={confirmDelete}
              disabled={deleteMutation.isPending}
            >
              {deleteMutation.isPending ? 'Deleting...' : 'Delete'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

function RootComponent() {
  return (
    <div className="h-screen flex">
      <SessionSidebar />
      <div className="flex-1 overflow-hidden">
        <Outlet />
      </div>
      <Toaster />
    </div>
  );
}
