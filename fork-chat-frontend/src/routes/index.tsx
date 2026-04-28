import { createFileRoute } from '@tanstack/react-router';

export const Route = createFileRoute('/')({
  component: () => (
    <div className="h-full flex items-center justify-center text-gray-400">
      Select a session from the sidebar or create a new one
    </div>
  ),
});
