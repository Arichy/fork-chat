import { createFileRoute } from '@tanstack/react-router';
import { ChatPage } from '../pages';

/**
 * URL search schema for the `/sessions/$sessionId` route.
 *
 * `turnId` drives which turn's detail modal is open. Persisting it in the URL
 * lets a page refresh (or a shared link) restore the same open-modal state, so
 * users don't lose context after an accidental reload.
 */
export type SessionSearch = {
  turnId?: string;
};

export const Route = createFileRoute('/sessions/$sessionId')({
  component: ChatPage,
  // Accept only well-formed string values and silently drop anything else.
  // This keeps malformed URLs (e.g. `?turnId[]=a`) from crashing the route.
  validateSearch: (search: Record<string, unknown>): SessionSearch => {
    const raw = search.turnId;
    return {
      turnId: typeof raw === 'string' && raw.length > 0 ? raw : undefined,
    };
  },
});
