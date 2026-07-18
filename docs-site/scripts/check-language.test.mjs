import { describe, expect, it } from 'vitest';
import {
  assertLocaleIsolation,
  findCjkOccurrences,
  findEnglishHeadingOccurrences,
  findEnglishProseOccurrences,
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

describe('findEnglishProseOccurrences', () => {
  it('reports a complete untranslated English paragraph inside a Chinese page', () => {
    expect(
      findEnglishProseOccurrences(
        'guide.md',
        '# 中文指南\n\nRead the current topology before issuing a command.\n'
      )
    ).toEqual([
      {
        path: 'guide.md',
        line: 3,
        text: 'Read the current topology before issuing a command.',
      },
    ]);
  });

  it('allows product names, protocol identifiers, and code examples', () => {
    const content = [
      '# Home Assistant 与 CloudLink',
      '',
      '`aether.cloudlink.integration.v1alpha1`',
      '',
      '```ts',
      '// Decode the provider response and return typed observations.',
      'const pointKey = "is_on";',
      '```',
      '',
    ].join('\n');
    expect(findEnglishProseOccurrences('guide.md', content)).toEqual([]);
  });
});

describe('findEnglishHeadingOccurrences', () => {
  it('reports untranslated English labels in Chinese headings', () => {
    expect(
      findEnglishHeadingOccurrences(
        'guide.md',
        '# 中文指南\n\n### Provenance\n\n### Channels create\n\n### `channels_create` (**WRITE**)\n'
      )
    ).toEqual([
      { path: 'guide.md', line: 3, text: '### Provenance' },
      { path: 'guide.md', line: 5, text: '### Channels create' },
      { path: 'guide.md', line: 7, text: '### `channels_create` (**WRITE**)' },
    ]);
  });

  it('allows product names, exact technical identifiers, commands, and fenced examples', () => {
    const content = [
      '# HTTP API',
      '',
      '## AetherCloud',
      '',
      '## aether status',
      '',
      '# aether-cloudlink',
      '',
      '## Docker Compose',
      '',
      '## RUSTSEC-2023-0071 (`rsa`)',
      '',
      '## `GET /health`',
      '',
      '## `ProcessingFrame`',
      '',
      '```markdown',
      '### English example heading',
      '```',
      '',
    ].join('\n');
    expect(findEnglishHeadingOccurrences('guide.md', content)).toEqual([]);
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

  it('rejects an English sentence even when the same Chinese page contains Chinese prose', () => {
    expect(() =>
      assertLocaleIsolation([
        {
          path: 'aethercloud/guide.md',
          content: '# 中文指南\n\n这是中文说明。\n\nUse the existing application boundary.\n',
        },
      ])
    ).toThrow(/untranslated English prose/);
  });

  it('rejects an untranslated English heading even when the page contains Chinese prose', () => {
    expect(() =>
      assertLocaleIsolation([
        {
          path: 'aethercloud/guide.md',
          content: '# 中文指南\n\n这是中文说明。\n\n### Schema\n',
        },
      ])
    ).toThrow(/untranslated English heading/);
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
