import '@testing-library/jest-dom/vitest';
import { cleanup } from '@testing-library/react';
import { afterEach } from 'vitest';

// Browser project setup: component tests mock the `@/api` module per-file with
// vi.mock, so we don't need an MSW worker here. We only wire jest-dom and
// auto-cleanup between tests.

afterEach(() => {
  cleanup();
});
