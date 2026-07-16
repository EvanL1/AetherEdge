import { describe, expect, it } from 'vitest';
import {
  assertUserFacingDocumentation,
  findInternalArchitectureReferences,
} from './check-audience.mjs';

describe('findInternalArchitectureReferences', () => {
  it('reports numbered decisions and internal ADR links', () => {
    expect(
      findInternalArchitectureReferences(
        'guide.md',
        'Read ADR-0009.\nSee https://example.com/docs/adr/0009-decision.md.\n'
      )
    ).toEqual([
      { path: 'guide.md', line: 1, text: 'Read ADR-0009.' },
      {
        path: 'guide.md',
        line: 2,
        text: 'See https://example.com/docs/adr/0009-decision.md.',
      },
    ]);
  });

  it('accepts user-facing architecture and compatibility links', () => {
    expect(
      findInternalArchitectureReferences(
        'guide.md',
        'Read the deployment guide and compatibility matrix.\n'
      )
    ).toEqual([]);
  });
});

describe('assertUserFacingDocumentation', () => {
  it('rejects maintainer-only references from either locale', () => {
    expect(() =>
      assertUserFacingDocumentation([
        { path: 'en/guide.md', content: 'See ADR-0012.\n' },
        { path: 'guide.md', content: '用户指南。\n' },
      ])
    ).toThrow(/en\/guide\.md:1/);
  });

  it('accepts documents written for product users', () => {
    expect(() =>
      assertUserFacingDocumentation([
        { path: 'en/guide.md', content: 'Read the compatibility guide.\n' },
        { path: 'guide.md', content: '请阅读兼容性指南。\n' },
      ])
    ).not.toThrow();
  });
});
