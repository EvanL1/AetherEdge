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

  it('rejects relative and site-root document links from published pages', () => {
    expect(() =>
      assertUserFacingDocumentation([
        {
          path: 'en/guide.md',
          content:
            'Read [relative](../reference/http-api.md) and [site-root](/en/reference/http-api).',
        },
      ])
    ).toThrow(/absolute documentation URL/);
  });

  it('rejects strong emphasis that CommonMark leaves as literal asterisks', () => {
    expect(() =>
      assertUserFacingDocumentation([
        {
          path: 'guide.md',
          content: '**成功标准：**运行命令。\n',
        },
      ])
    ).toThrow(/unrendered Markdown emphasis/);
  });

  it('accepts unambiguous Chinese emphasis and literal markers in code', () => {
    expect(() =>
      assertUserFacingDocumentation([
        {
          path: 'guide.md',
          content:
            '**成功标准**：运行命令。\n\n**成功标准：** 运行命令。\n\n使用 `**` 表示粗体。\n',
        },
      ])
    ).not.toThrow();
  });

  it('rejects absolute documentation URLs that do not resolve to a published route', () => {
    expect(() =>
      assertUserFacingDocumentation([
        {
          path: 'en/guide.md',
          content:
            'Read [missing](https://docs.aetheriot.workers.dev/en/reference/not-published).\n',
        },
      ])
    ).toThrow(/published documentation route/);
  });

  it('rejects accidental cross-locale links but allows explicit language switches', () => {
    const chineseTarget = {
      path: 'compatibility/version-matrix.md',
      content: '中文兼容性说明。\n',
    };

    expect(() =>
      assertUserFacingDocumentation([
        {
          path: 'en/guide.md',
          content:
            'Read [Compatibility](https://docs.aetheriot.workers.dev/compatibility/version-matrix).\n',
        },
        chineseTarget,
      ])
    ).toThrow(/wrong documentation locale/);

    expect(() =>
      assertUserFacingDocumentation([
        {
          path: 'en/guide.md',
          content:
            'Read the [Simplified Chinese version](https://docs.aetheriot.workers.dev/compatibility/version-matrix).\n',
        },
        chineseTarget,
      ])
    ).not.toThrow();
  });

  it('accepts documents written for product users', () => {
    expect(() =>
      assertUserFacingDocumentation([
        {
          path: 'en/guide.md',
          content:
            'Read the [compatibility guide](https://docs.aetheriot.workers.dev/en/compatibility/version-matrix).\n',
        },
        {
          path: 'guide.md',
          content:
            '请阅读[兼容性指南](https://docs.aetheriot.workers.dev/compatibility/version-matrix)。\n',
        },
        {
          path: 'en/compatibility/version-matrix.md',
          content: 'English compatibility guide.\n',
        },
        {
          path: 'compatibility/version-matrix.md',
          content: '中文兼容性说明。\n',
        },
      ])
    ).not.toThrow();
  });
});
