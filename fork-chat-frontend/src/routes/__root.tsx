import {
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query';
import {
  createRootRoute,
  Link,
  Outlet,
  useNavigate,
  useParams,
} from '@tanstack/react-router';
import {
  Ellipsis,
  MessageSquare,
  PanelLeftClose,
  PanelLeftOpen,
  Pencil,
  Plus,
} from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { Toaster } from 'sonner';
import { api } from '../api';
import type { Protocol, SessionsSort } from '../api/types';
import { Button } from '../components/ui/button';
import { Checkbox } from '../components/ui/checkbox';
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
import { Input } from '../components/ui/input';
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
  updated_at: string;
};

type SessionGroupLabel = 'Today' | 'Yesterday' | 'Previous 7 days' | 'Older';

const GROUP_ORDER: SessionGroupLabel[] = [
  'Today',
  'Yesterday',
  'Previous 7 days',
  'Older',
];
const SESSIONS_PAGE_SIZE = 20;
const SESSION_FILTER_DEBOUNCE_MS = 300;
const SESSION_SORT_OPTIONS: Array<{ value: SessionsSort; label: string }> = [
  { value: 'updated_at', label: 'Recent Activity' },
  { value: 'created_at', label: 'Recently Created' },
];

function startOfDay(d: Date): number {
  const copy = new Date(d);
  copy.setHours(0, 0, 0, 0);
  return copy.getTime();
}

function groupSessions(
  sessions: SessionSummary[],
  sort: SessionsSort,
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
    const ts = startOfDay(
      new Date(sort === 'updated_at' ? s.updated_at : s.created_at),
    );
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

export function SessionSidebar() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  // Read the current session id from the active route match. `strict: false`
  // means this returns the params of whatever route is currently matched,
  // so `sessionId` is present on `/sessions/$sessionId` and undefined otherwise.
  // This replaces ad-hoc `window.location.pathname` parsing, which can
  // mis-handle trailing slashes or query strings.
  const routeParams = useParams({ strict: false }) as {
    sessionId?: string;
  };
  const currentSessionId = routeParams.sessionId;
  const { setSelectedTurn } = useChatStore();
  const [collapsed, setCollapsed] = useState(false);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState('');
  const [pendingDelete, setPendingDelete] = useState<{
    id: string;
    title: string;
  } | null>(null);
  const [titleFilter, setTitleFilter] = useState('');
  const [debouncedTitleFilter, setDebouncedTitleFilter] = useState('');
  const [sortBy, setSortBy] = useState<SessionsSort>('updated_at');
  const listRef = useRef<HTMLDivElement | null>(null);
  // Keep the last title displayed while the dialog animates closed,
  // so cancel/confirm doesn't show an empty title flash.
  const lastDeleteTitleRef = useRef<string>('');
  if (pendingDelete) lastDeleteTitleRef.current = pendingDelete.title;
  const deleteDialogTitle = pendingDelete?.title ?? lastDeleteTitleRef.current;

  // Selection mode state for batch operations.
  const [isSelectionMode, setIsSelectionMode] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [pendingBatchDeleteCount, setPendingBatchDeleteCount] = useState<
    number | null
  >(null);
  useEffect(() => {
    const timer = setTimeout(() => {
      setDebouncedTitleFilter(titleFilter);
    }, SESSION_FILTER_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [titleFilter]);

  const normalizedFilter = debouncedTitleFilter.trim();

  const {
    data: sessionsPages,
    isLoading,
    error,
    hasNextPage,
    isFetchingNextPage,
    fetchNextPage,
  } = useInfiniteQuery({
    queryKey: ['sessions', 'pages', sortBy, normalizedFilter],
    initialPageParam: null as {
      before_at: string;
      before_id: string;
    } | null,
    queryFn: ({ pageParam }) =>
      api.sessions.list({
        limit: SESSIONS_PAGE_SIZE,
        cursor: pageParam,
        sort: sortBy,
        filter: normalizedFilter,
      }),
    getNextPageParam: (lastPage) => lastPage.next_cursor,
    staleTime: Infinity,
  });

  const allSessions = useMemo(
    () => sessionsPages?.pages.flatMap((page) => page.sessions) ?? [],
    [sessionsPages],
  );

  useEffect(() => {
    const el = listRef.current;
    if (!el || !hasNextPage || isFetchingNextPage) return;
    const loadedCount = allSessions.length;
    if (loadedCount === 0 && !hasNextPage) return;
    // If viewport isn't filled yet, keep pulling the next page.
    if (el.scrollHeight <= el.clientHeight + 64) {
      void fetchNextPage();
    }
  }, [allSessions.length, fetchNextPage, hasNextPage, isFetchingNextPage]);

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

  const batchDeleteMutation = useMutation({
    mutationFn: (ids: string[]) => api.sessions.batchDelete(ids),
    onSuccess: (_result, deletedIds) => {
      qc.invalidateQueries({ queryKey: ['sessions'] });
      setSelectedTurn(null);
      // If the currently-viewed session was in the deleted batch, jump to home
      // so the user isn't left on a 404'd session detail page. `currentSessionId`
      // comes from the router's matched params (see `useParams` above), which is
      // always in sync with the active route — no URL parsing required.
      if (currentSessionId && deletedIds.includes(currentSessionId)) {
        navigate({ to: '/' });
      }
      // Exit selection mode after successful delete.
      setIsSelectionMode(false);
      setSelectedIds(new Set());
      setPendingBatchDeleteCount(null);
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

  const handleBatchDelete = () => {
    if (selectedIds.size > 0) {
      setPendingBatchDeleteCount(selectedIds.size);
    }
  };

  const confirmBatchDelete = () => {
    if (pendingBatchDeleteCount !== null) {
      batchDeleteMutation.mutate(Array.from(selectedIds));
    }
  };

  const exitSelectionMode = () => {
    setIsSelectionMode(false);
    setSelectedIds(new Set());
  };

  const toggleSelectAll = () => {
    if (selectedIds.size === allSessions.length) {
      setSelectedIds(new Set());
    } else {
      setSelectedIds(new Set(allSessions.map((s) => s.id)));
    }
  };

  const toggleSelectSession = (id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
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
          onClick={() => {
            setIsSelectionMode(!isSelectionMode);
            if (isSelectionMode) setSelectedIds(new Set());
          }}
          className={`p-1.5 rounded-md mb-2 transition-colors cursor-pointer ${
            isSelectionMode
              ? 'text-zinc-100 bg-zinc-700'
              : 'text-zinc-400 hover:text-zinc-100 hover:bg-zinc-800'
          }`}
          title={isSelectionMode ? 'Exit selection' : 'Batch select'}
        >
          <Pencil className="size-4" />
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
      <div className="p-3 border-b border-zinc-800">
        <div className="flex items-center justify-between">
          <h1 className="font-semibold tracking-tight">Fork Chat</h1>
          <div className="flex items-center gap-1">
            <button
              type="button"
              onClick={() => {
                setIsSelectionMode(!isSelectionMode);
                if (isSelectionMode) setSelectedIds(new Set());
              }}
              className={`p-1 rounded-md transition-colors cursor-pointer ${
                isSelectionMode
                  ? 'text-zinc-100 bg-zinc-700'
                  : 'text-zinc-400 hover:text-zinc-100 hover:bg-zinc-800'
              }`}
              title={isSelectionMode ? 'Exit selection' : 'Batch select'}
            >
              <Pencil className="size-4" />
            </button>
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
        <div className="mt-2 space-y-1.5">
          <p className="px-0.5 text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
            Protocol (for new session)
          </p>
          <div className="flex items-center gap-2">
            <Select
              value={newSessionProtocol}
              onValueChange={(v) => setNewSessionProtocol(v as Protocol)}
            >
              <SelectTrigger
                className="h-8 flex-1 rounded-xl border border-zinc-700/80 bg-zinc-800/90 px-3 text-xs font-medium text-zinc-100 shadow-inner shadow-black/20 transition-colors hover:bg-zinc-700/90 focus-visible:border-zinc-500 focus-visible:ring-2 focus-visible:ring-zinc-500/40 data-[popup-open]:bg-zinc-700/90 [&_svg]:text-zinc-400"
                size="sm"
                aria-label="Protocol for new session"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent
                sideOffset={6}
                align="start"
                alignItemWithTrigger={false}
                className="w-[--anchor-width] min-w-[--anchor-width] rounded-xl border border-zinc-700 bg-zinc-900/95 p-1 text-zinc-100 shadow-2xl shadow-black/45 backdrop-blur-sm"
              >
                {availableProtocols.map((p) => (
                  <SelectItem
                    key={p}
                    value={p}
                    className="rounded-lg py-1.5 pr-8 pl-3 text-sm font-medium text-zinc-200 focus:bg-zinc-800 focus:!text-zinc-50 data-[selected]:bg-zinc-800/80 data-[selected]:!text-zinc-50 data-[highlighted]:bg-zinc-800 data-[highlighted]:!text-zinc-50 [&_svg]:text-zinc-300 data-[selected]:[&_svg]:text-zinc-100 data-[highlighted]:[&_svg]:text-zinc-100"
                  >
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
              className="shrink-0"
            >
              <Plus className="size-3.5" />
            </Button>
          </div>
        </div>
        {isSelectionMode ? (
          <div className="mt-2 flex items-center justify-between">
            <Button
              variant="ghost"
              size="sm"
              onClick={exitSelectionMode}
              className="text-xs text-zinc-400 hover:text-zinc-100 px-1"
            >
              Cancel
            </Button>
            <span className="text-xs text-zinc-400">
              {selectedIds.size} selected
            </span>
          </div>
        ) : (
          <>
            <div className="mt-2 space-y-1.5">
              <p className="px-0.5 text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
                Sort
              </p>
              <Select
                value={sortBy}
                onValueChange={(v) => setSortBy(v as SessionsSort)}
              >
                <SelectTrigger
                  className="h-8 rounded-xl border border-zinc-700/80 bg-zinc-800/90 px-3 text-xs font-medium text-zinc-100 shadow-inner shadow-black/20 transition-colors hover:bg-zinc-700/90 focus-visible:border-zinc-500 focus-visible:ring-2 focus-visible:ring-zinc-500/40"
                  size="sm"
                  aria-label="Session list sort"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent
                  sideOffset={6}
                  align="start"
                  alignItemWithTrigger={false}
                  className="w-[--anchor-width] min-w-[--anchor-width] rounded-xl border border-zinc-700 bg-zinc-900/95 p-1 text-zinc-100 shadow-2xl shadow-black/45 backdrop-blur-sm"
                >
                  {SESSION_SORT_OPTIONS.map((opt) => (
                    <SelectItem
                      key={opt.value}
                      value={opt.value}
                      className="rounded-lg py-1.5 pr-8 pl-3 text-sm font-medium text-zinc-200 focus:bg-zinc-800 focus:!text-zinc-50 data-[selected]:bg-zinc-800/80 data-[selected]:!text-zinc-50 data-[highlighted]:bg-zinc-800 data-[highlighted]:!text-zinc-50 [&_svg]:text-zinc-300 data-[selected]:[&_svg]:text-zinc-100 data-[highlighted]:[&_svg]:text-zinc-100"
                    >
                      {opt.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="mt-2 space-y-1.5">
              <p className="px-0.5 text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
                Filter (title)
              </p>
              <Input
                value={titleFilter}
                onChange={(e) => setTitleFilter(e.target.value)}
                placeholder="Filter titles..."
                className="h-8 rounded-xl border border-zinc-700/80 bg-zinc-800/80 px-3 text-xs text-zinc-100 placeholder:text-zinc-500 focus-visible:border-zinc-500 focus-visible:ring-2 focus-visible:ring-zinc-500/30"
              />
            </div>
          </>
        )}
      </div>

      <div
        ref={listRef}
        className="sidebar-scrollbar flex-1 overflow-auto px-2 py-2"
        onScroll={() => {
          const el = listRef.current;
          if (!el || !hasNextPage || isFetchingNextPage) return;
          const remaining = el.scrollHeight - el.scrollTop - el.clientHeight;
          if (remaining < 96) {
            void fetchNextPage();
          }
        }}
      >
        {isLoading && (
          <div className="p-3 text-xs text-zinc-500">Loading...</div>
        )}
        {error && (
          <div className="p-3 text-xs text-red-400">
            Error: {error instanceof Error ? error.message : 'Failed to load'}
          </div>
        )}

        {allSessions.length > 0 && isSelectionMode && (
          <div className="flex items-center gap-2 px-2.5 py-1 mb-1">
            <Checkbox
              checked={
                allSessions.length > 0 &&
                selectedIds.size === allSessions.length
              }
              onCheckedChange={toggleSelectAll}
              className="border-zinc-600 data-checked:bg-zinc-100 data-checked:border-zinc-100"
            />
            <span className="text-[11px] text-zinc-500">Select all</span>
          </div>
        )}

        {allSessions.length > 0 &&
          (() => {
            const groups = groupSessions(allSessions, sortBy);
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
                          {isSelectionMode ? (
                            <Checkbox
                              checked={selectedIds.has(session.id)}
                              onCheckedChange={() =>
                                toggleSelectSession(session.id)
                              }
                              onClick={(e) => e.preventDefault()}
                              className="border-zinc-600 data-checked:bg-zinc-100 data-checked:border-zinc-100 shrink-0"
                            />
                          ) : (
                            <MessageSquare className="size-3.5 shrink-0 text-zinc-500 group-hover/row:text-zinc-400" />
                          )}
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
                              {(() => {
                                const rowTs =
                                  sortBy === 'updated_at'
                                    ? session.updated_at
                                    : session.created_at;
                                return (
                                  <span
                                    className="shrink-0 text-[11px] tabular-nums text-zinc-500 group-hover/row:text-zinc-400 transition-colors"
                                    title={new Date(rowTs).toLocaleString()}
                                  >
                                    {formatRowTimestamp(rowTs, group)}
                                  </span>
                                );
                              })()}
                            </>
                          )}
                        </Link>

                        {!isSelectionMode && (
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
                        )}
                      </div>
                    ))}
                  </div>
                </div>
              ),
            );
          })()}

        {allSessions.length === 0 && !isLoading && !error && (
          <div className="p-4 text-zinc-500 text-center text-sm">
            {normalizedFilter.length > 0
              ? 'No sessions match this title filter.'
              : 'No sessions yet. Click + to start one.'}
          </div>
        )}
        {isFetchingNextPage && (
          <div className="p-2 text-center text-[11px] text-zinc-500">
            Loading more sessions...
          </div>
        )}
      </div>

      {isSelectionMode && selectedIds.size > 0 && (
        <div className="p-3 border-t border-zinc-800 flex items-center justify-between bg-zinc-900">
          <span className="text-xs text-zinc-400">
            {selectedIds.size} selected
          </span>
          <Button
            variant="destructive"
            size="sm"
            onClick={handleBatchDelete}
            className="text-xs"
          >
            Delete {selectedIds.size}
          </Button>
        </div>
      )}

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

      <Dialog
        open={pendingBatchDeleteCount !== null}
        onOpenChange={(open) => {
          if (!open) setPendingBatchDeleteCount(null);
        }}
      >
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>
              Delete {pendingBatchDeleteCount} sessions?
            </DialogTitle>
            <DialogDescription>
              {pendingBatchDeleteCount} sessions and all of their messages will
              be permanently removed. This action cannot be undone.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="secondary"
              onClick={() => setPendingBatchDeleteCount(null)}
            >
              Cancel
            </Button>
            <Button
              variant="destructive"
              onClick={confirmBatchDelete}
              disabled={batchDeleteMutation.isPending}
            >
              {batchDeleteMutation.isPending ? 'Deleting...' : 'Delete'}
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
      <Toaster richColors />
    </div>
  );
}
