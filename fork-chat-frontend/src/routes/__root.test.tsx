import { HttpResponse, http } from 'msw';
import { describe, expect, it, vi } from 'vitest';
import { server } from '../test/server-from-setup';
import { renderWithProviders } from '../test/render';
import { makeSession } from '../test/fixtures';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { SessionSidebar } from './__root';

const API_BASE = 'http://localhost:3000/api';

// Helper: configure MSW to return specific sessions for the list endpoint.
function givenSessions(count: number) {
  const sessions = Array.from({ length: count }, (_, i) =>
    makeSession({
      id: `session-${i + 1}`,
      title: `Session ${i + 1}`,
      updated_at: new Date(Date.now() - i * 3600_000).toISOString(),
    }),
  );
  server.use(
    http.get(`${API_BASE}/sessions`, () =>
      HttpResponse.json({ sessions, next_cursor: null }),
    ),
  );
}

// Mock TanStack Router's useNavigate since SessionSidebar depends on it.
vi.mock('@tanstack/react-router', async (importOriginal) => {
  const actual =
    await importOriginal<typeof import('@tanstack/react-router')>();
  return {
    ...actual,
    useNavigate: () => vi.fn(),
    // SessionSidebar calls `useParams({ strict: false })` to read the current
    // session id from the active route. Tests render the sidebar without a
    // real Router, so return an empty params object by default.
    useParams: () => ({}),
    Link: ({
      children,
      ...props
    }: {
      children: React.ReactNode;
      to: string;
      params: Record<string, string>;
      className?: string;
      activeProps?: Record<string, unknown>;
      onClick?: () => void;
    }) => <a {...props}>{children}</a>,
  };
});

describe('SessionSidebar batch selection', () => {
  it('renders the edit/pencil button', async () => {
    givenSessions(2);
    renderWithProviders(<SessionSidebar />);
    expect(await screen.findByTitle('Batch select')).toBeInTheDocument();
  });

  it('enters selection mode when edit button is clicked', async () => {
    const user = userEvent.setup();
    givenSessions(2);
    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    // "Select all" label should appear.
    expect(screen.getByText('Select all')).toBeInTheDocument();
    // "Cancel" button should appear in the header.
    expect(screen.getByText('Cancel')).toBeInTheDocument();
    // Each session row should now have a checkbox (2 rows + 1 "Select all").
    const checkboxes = screen.getAllByRole('checkbox');
    expect(checkboxes.length).toBe(3);
  });

  it('selects a session when its checkbox is clicked', async () => {
    const user = userEvent.setup();
    givenSessions(2);
    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    // Click the checkbox for the first session row (second checkbox overall,
    // since the first is "Select all").
    const checkboxes = screen.getAllByRole('checkbox');
    await user.click(checkboxes[1]);

    expect(screen.getAllByText('1 selected').length).toBeGreaterThanOrEqual(1);
  });

  it('selects all sessions when "Select all" is clicked', async () => {
    const user = userEvent.setup();
    givenSessions(3);
    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    // Click "Select all" (first checkbox).
    const selectAllCheckbox = screen.getAllByRole('checkbox')[0];
    await user.click(selectAllCheckbox);

    // "3 selected" appears in both header and bottom bar.
    expect(screen.getAllByText('3 selected').length).toBeGreaterThanOrEqual(1);
  });

  it('deselects all when "Select all" is clicked twice', async () => {
    const user = userEvent.setup();
    givenSessions(2);
    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    const selectAllCheckbox = screen.getAllByRole('checkbox')[0];
    await user.click(selectAllCheckbox);
    expect(screen.getAllByText('2 selected').length).toBeGreaterThanOrEqual(1);

    await user.click(selectAllCheckbox);
    // After deselecting, "0 selected" appears in the header (bottom bar is gone).
    expect(screen.getByText('0 selected')).toBeInTheDocument();
  });

  it('shows "Delete N" button in bottom bar when sessions are selected', async () => {
    const user = userEvent.setup();
    givenSessions(2);
    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    // Select all.
    const selectAllCheckbox = screen.getAllByRole('checkbox')[0];
    await user.click(selectAllCheckbox);

    expect(screen.getByText('Delete 2')).toBeInTheDocument();
  });

  it('opens confirmation dialog when "Delete N" is clicked', async () => {
    const user = userEvent.setup();
    givenSessions(2);
    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    const selectAllCheckbox = screen.getAllByRole('checkbox')[0];
    await user.click(selectAllCheckbox);

    const deleteBtn = screen.getByText('Delete 2');
    await user.click(deleteBtn);

    expect(screen.getByText('Delete 2 sessions?')).toBeInTheDocument();
  });

  it('calls batch delete API on confirm', async () => {
    const user = userEvent.setup();
    givenSessions(2);

    let capturedBody: unknown = null;
    server.use(
      http.post(`${API_BASE}/sessions/batch-delete`, async ({ request }) => {
        capturedBody = await request.json();
        return HttpResponse.json({ deleted: 2 });
      }),
    );

    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    const selectAllCheckbox = screen.getAllByRole('checkbox')[0];
    await user.click(selectAllCheckbox);

    const deleteBtn = screen.getByText('Delete 2');
    await user.click(deleteBtn);

    // Confirm in dialog — find the dialog's Delete button (it's the
    // destructive one inside the dialog footer).
    const confirmBtn = screen
      .getAllByRole('button')
      .find(
        (btn) => btn.textContent === 'Delete' && btn.closest('[role="dialog"]'),
      )!;
    await user.click(confirmBtn);

    await waitFor(() => {
      expect(capturedBody).toEqual({
        ids: ['session-1', 'session-2'],
      });
    });
  });

  it('closes dialog on cancel without calling API', async () => {
    const user = userEvent.setup();
    givenSessions(2);

    const batchDeleteSpy = vi.fn();
    server.use(
      http.post(`${API_BASE}/sessions/batch-delete`, () => {
        batchDeleteSpy();
        return HttpResponse.json({ deleted: 2 });
      }),
    );

    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    const selectAllCheckbox = screen.getAllByRole('checkbox')[0];
    await user.click(selectAllCheckbox);

    const deleteBtn = screen.getByText('Delete 2');
    await user.click(deleteBtn);

    // Cancel in dialog — find the Cancel button inside the dialog.
    const cancelBtn = screen
      .getAllByRole('button')
      .find(
        (btn) => btn.textContent === 'Cancel' && btn.closest('[role="dialog"]'),
      )!;
    await user.click(cancelBtn);

    // Dialog should be closed.
    expect(screen.queryByText('Delete 2 sessions?')).not.toBeInTheDocument();
    expect(batchDeleteSpy).not.toHaveBeenCalled();
    // Selections should remain ("2 selected" appears in header + bottom bar).
    expect(screen.getAllByText('2 selected').length).toBeGreaterThanOrEqual(1);
  });

  it('exits selection mode when cancel button is clicked', async () => {
    const user = userEvent.setup();
    givenSessions(2);
    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    // Cancel in the selection header.
    const cancelBtn = screen.getByText('Cancel');
    await user.click(cancelBtn);

    // Selection mode UI should be gone.
    expect(screen.queryByText('Select all')).not.toBeInTheDocument();
    // Sort controls should be back.
    expect(screen.getByText('Sort')).toBeInTheDocument();
  });
});
