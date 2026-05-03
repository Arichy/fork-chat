import '@testing-library/jest-dom/vitest';
import { cleanup } from '@testing-library/react';
import { afterAll, afterEach, beforeAll } from 'vitest';
import { server } from './msw/server';

// Polyfill PointerEvent for jsdom — @base-ui/react components (Checkbox, etc.)
// reference PointerEvent which jsdom doesn't provide.
if (typeof PointerEvent === 'undefined') {
  class PointerEventPolyfill extends MouseEvent {
    public pointerId: number;
    public width: number;
    public height: number;
    public pressure: number;
    public tangentialPressure: number;
    public tiltX: number;
    public tiltY: number;
    public twist: number;
    public pointerType: string;
    public isPrimary: boolean;

    constructor(type: string, eventInit: PointerEventInit = {}) {
      super(type, eventInit);
      this.pointerId = eventInit.pointerId ?? 0;
      this.width = eventInit.width ?? 1;
      this.height = eventInit.height ?? 1;
      this.pressure = eventInit.pressure ?? 0;
      this.tangentialPressure = eventInit.tangentialPressure ?? 0;
      this.tiltX = eventInit.tiltX ?? 0;
      this.tiltY = eventInit.tiltY ?? 0;
      this.twist = eventInit.twist ?? 0;
      this.pointerType = eventInit.pointerType ?? 'mouse';
      this.isPrimary = eventInit.isPrimary ?? false;
    }
  }
  Object.defineProperty(globalThis, 'PointerEvent', {
    value: PointerEventPolyfill,
    writable: true,
  });
}

// Node (jsdom) project setup: MSW server intercepts fetch, jest-dom adds
// matchers, and React Testing Library resets the DOM between tests.

beforeAll(() => {
  server.listen({ onUnhandledRequest: 'error' });
});

afterEach(() => {
  cleanup();
  server.resetHandlers();
});

afterAll(() => {
  server.close();
});
