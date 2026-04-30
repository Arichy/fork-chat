// Re-export the MSW server singleton. Setup file already starts it for the
// Node project. Individual tests import `server` to call `.use()` for overrides.
export { server } from './msw/server';
