import { describe, expect, it } from 'vitest';
import {
  assertLocaleIsolation,
  findCjkOccurrences,
  localeForPath,
} from './check-language.mjs';

describe('findCjkOccurrences', () => {
  it('reports the source path, line, and offending text', () => {
    expect(findCjkOccurrences('guide.md', '# Guide\n\n这是中文。\n')).toEqual([
      { path: 'guide.md', line: 3, text: '这是中文。' },
    ]);
  });

  it('accepts English Markdown with Unicode punctuation', () => {
    expect(findCjkOccurrences('guide.md', '# Guide — Aether\n\nIt’s agent-native.\n')).toEqual([]);
  });
});

describe('localeForPath', () => {
  it('treats /en as English and the root locale as Simplified Chinese', () => {
    expect(localeForPath('en/guides/getting-started.md')).toBe('en');
    expect(localeForPath('en.md')).toBe('en');
    expect(localeForPath('aethercontracts/getting-started.md')).toBe('zh-CN');
  });
});

describe('assertLocaleIsolation', () => {
  it('rejects CJK text from the English publication', () => {
    expect(() =>
      assertLocaleIsolation([
        { path: 'en/first.md', content: 'English.\n中文。\n' },
      ])
    ).toThrow(/en\/first\.md:2/);
  });

  it('rejects untranslated prose from the Chinese publication', () => {
    expect(() =>
      assertLocaleIsolation([
        { path: 'aethercloud/guide.md', content: '# Cloud guide\n\nEnglish only.\n' },
      ])
    ).toThrow(/Chinese publication/);
  });

  it('accepts isolated Chinese and English documents', () => {
    expect(() =>
      assertLocaleIsolation([
        { path: 'guide.md', content: '# 中文指南\n\n使用 AetherEdge。\n' },
        { path: 'en/guide.md', content: '# English guide\n\nUse AetherEdge.\n' },
      ])
    ).not.toThrow();
  });
});
