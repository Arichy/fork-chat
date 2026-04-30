import { describe, expect, it } from 'vitest';
import { cn } from './utils';

describe('cn', () => {
  it('merges conflicting Tailwind utilities, last wins', () => {
    expect(cn('p-2', 'p-4')).toBe('p-4');
    expect(cn('text-red-500', 'text-blue-500')).toBe('text-blue-500');
  });

  it('filters out falsy values', () => {
    expect(cn('a', false, null, undefined, '', 'b')).toBe('a b');
  });

  it('handles arrays of classes', () => {
    expect(cn(['a', 'b'], 'c')).toBe('a b c');
  });

  it('handles object syntax from clsx', () => {
    expect(cn({ foo: true, bar: false })).toBe('foo');
  });

  it('returns empty string when no inputs provided', () => {
    expect(cn()).toBe('');
  });

  it('preserves non-conflicting Tailwind classes', () => {
    expect(cn('p-4 text-sm', 'font-bold')).toBe('p-4 text-sm font-bold');
  });
});
