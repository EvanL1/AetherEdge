import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const docsSiteRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
);

function read(relativePath) {
  return readFileSync(path.join(docsSiteRoot, relativePath), "utf8");
}

describe("AetherIoT documentation brand system", () => {
  it("shares the website color, type, and geometry tokens", () => {
    const styles = read("src/styles/custom.css");

    expect(styles).toContain("--aether-ink: #07110f");
    expect(styles).toContain("--aether-ink-soft: #0d1a17");
    expect(styles).toContain("--aether-paper: #f1f4ed");
    expect(styles).toContain("--aether-paper-strong: #fbfcf8");
    expect(styles).toContain("--aether-mint: #b8ff62");
    expect(styles).toContain("--aether-mint-soft: #d9ffab");
    expect(styles).toContain("--aether-amber: #ffbd59");
    expect(styles).toContain("--aether-muted: #9aaba5");
    expect(styles).toContain("--aether-line: rgba(241, 244, 237, 0.16)");
    expect(styles).toContain("--aether-line-dark: rgba(7, 17, 15, 0.14)");
    expect(styles).toMatch(
      /@import ["']@fontsource-variable\/geist\/wght\.css["']/,
    );
    expect(styles).toMatch(
      /@import ["']@fontsource-variable\/geist-mono\/wght\.css["']/,
    );
    expect(styles).toMatch(/--sl-font:\s*["']Geist Variable["']/);
    expect(styles).toContain("PingFang SC");
    expect(styles).toContain("Microsoft YaHei");
    expect(styles).toContain("Noto Sans CJK SC");
    expect(styles).toMatch(/--sl-font-mono:\s*["']Geist Mono Variable["']/);
    expect(styles).toMatch(
      /body\s*{[\s\S]*linear-gradient\([\s\S]*background-size:\s*68px 68px/,
    );
  });

  it("keeps branded dark and light reading surfaces accessible", () => {
    const styles = read("src/styles/custom.css");

    expect(styles).toMatch(/:root\[data-theme=["']light["']\]/);
    expect(styles).toContain("--sl-color-bg: var(--aether-paper-strong)");
    expect(styles).toContain("--sl-color-text-accent: #47710f");
    expect(styles).toMatch(
      /:root:not\(\[data-theme=["']light["']\]\)[\s\S]*--sl-color-bg:\s*var\(--aether-ink\)/,
    );
  });

  it("starts new visitors in the website dark theme without removing saved theme choices", () => {
    const config = read("astro.config.mjs");
    const provider = read("src/components/ThemeProvider.astro");

    expect(config).toContain(
      "ThemeProvider: './src/components/ThemeProvider.astro'",
    );
    expect(provider).toContain("const preferenceKey = 'starlight-theme'");
    expect(provider).toContain(
      "const initializedKey = 'aether-theme-initialized'",
    );
    expect(provider).toContain("if (storedTheme === null)");
    expect(provider).toContain("localStorage.setItem(preferenceKey, 'dark')");
    expect(provider).toContain(
      "window.matchMedia('(prefers-color-scheme: light)')",
    );
    expect(provider).toContain("document.documentElement.dataset.theme");
    expect(provider).toContain("StarlightThemeProvider");
  });

  it("uses the same brand mark and square component language as the website", () => {
    const styles = read("src/styles/custom.css");
    const favicon = read("public/favicon.svg");

    expect(styles).toMatch(/--sl-nav-height:\s*5\.25rem/);
    expect(styles).toMatch(/\.site-title::before[\s\S]*content:\s*["']A["']/);
    expect(styles).toMatch(
      /\.site-title::before[\s\S]*font-family:\s*var\(--__sl-font-mono\)/,
    );
    expect(styles).not.toMatch(
      /\.site-title::before[\s\S]*background-image:\s*url\(["']data:image\/svg\+xml/,
    );
    expect(styles).toMatch(/\.sidebar-content a\s*{[\s\S]*border-radius:\s*0/);
    expect(styles).toMatch(/site-search dialog[\s\S]*border-radius:\s*0/);
    expect(styles).toMatch(
      /starlight-menu-button button[\s\S]*border-radius:\s*0/,
    );
    expect(styles).toMatch(
      /#starlight__search\s*{[\s\S]*--sl-search-corners:\s*0[\s\S]*--pagefind-ui-border-radius:\s*0[\s\S]*--pagefind-ui-image-border-radius:\s*0/,
    );
    expect(styles).toMatch(
      /mobile-starlight-toc \.toggle[\s\S]*border-radius:\s*0/,
    );
    expect(styles).toMatch(/\.pagination-links a\s*{[\s\S]*border-radius:\s*0/);
    expect(styles).toMatch(/\.pagination-links a\s*{[\s\S]*box-shadow:\s*none/);
    expect(favicon).toContain('fill="#07110f"');
    expect(favicon).toContain('stroke="#b8ff62"');
  });

  it("keeps the documentation hero proportional on wide screens", () => {
    const styles = read("src/styles/custom.css");

    expect(styles).toContain(
      "--aether-page-gutter: clamp(1rem, 2.5vw, 3rem)",
    );
    expect(styles).toMatch(
      /\.content-panel \.hero\s*{[^}]*min-height:\s*0;[^}]*padding-block:\s*clamp\(3\.5rem,\s*8vh,\s*5rem\);[^}]*padding-inline:\s*var\(--aether-page-gutter\)/,
    );
    expect(styles).toMatch(
      /\.content-panel \.hero \.stack\s*{[^}]*max-width:\s*48rem/,
    );
    expect(styles).toMatch(
      /\.content-panel \.hero h1\s*{[\s\S]*font-size:\s*clamp\(2\.875rem,\s*4\.25vw,\s*4\.75rem\)/,
    );
    expect(styles).toMatch(
      /\.content-panel \.hero \.tagline\s*{[\s\S]*font-size:\s*clamp\(1\.0625rem,\s*1\.7vw,\s*1\.3125rem\)/,
    );
  });

  it("localizes framework-owned controls for Chinese readers", () => {
    const astroConfig = read("astro.config.mjs");
    const contentConfig = read("src/content.config.ts");
    const chineseTranslations = JSON.parse(read("src/content/i18n/zh-CN.json"));
    const englishTranslations = JSON.parse(read("src/content/i18n/en.json"));

    expect(astroConfig).toContain("expressiveCode:");
    expect(astroConfig).toContain(
      "return /(^|\\/)en(?:\\/|$)/.test(sourcePath) ? 'en' : 'zh-CN'",
    );
    expect(contentConfig).toContain(
      "import { docsLoader, i18nLoader } from '@astrojs/starlight/loaders'",
    );
    expect(contentConfig).toContain(
      "import { docsSchema, i18nSchema } from '@astrojs/starlight/schema'",
    );
    expect(contentConfig).toContain(
      "i18n: defineCollection({ loader: i18nLoader(), schema: i18nSchema() })",
    );
    expect(chineseTranslations).toMatchObject({
      "expressiveCode.copyButtonTooltip": "复制到剪贴板",
      "expressiveCode.copyButtonCopied": "已复制！",
      "expressiveCode.terminalWindowFallbackTitle": "终端窗口",
      "heading.anchorLabel": "“{{title}}”标题的链接",
    });
    expect(englishTranslations).toMatchObject({
      "expressiveCode.copyButtonTooltip": "Copy to clipboard",
      "expressiveCode.copyButtonCopied": "Copied!",
      "expressiveCode.terminalWindowFallbackTitle": "Terminal window",
      "heading.anchorLabel": "Section titled “{{title}}”",
    });
  });

  it("replaces generic code and aside colors with the AetherIoT palette", () => {
    const styles = read("src/styles/custom.css");

    expect(styles).toMatch(
      /:root:not\(\[data-theme=["']light["']\]\) \.expressive-code[\s\S]*--ec-codeBg:\s*#040b09/,
    );
    expect(styles).toMatch(
      /:root:not\(\[data-theme=["']light["']\]\) \.expressive-code[\s\S]*--ec-frm-edBg:\s*#040b09/,
    );
    expect(styles).toMatch(
      /:root\[data-theme=["']light["']\] \.expressive-code:not\(\[data-theme=["']dark["']\]\)[\s\S]*--ec-codeBg:\s*#e7ebe3/,
    );
    expect(styles).toMatch(
      /\.starlight-aside--note,[\s\S]*\.starlight-aside--tip[\s\S]*--sl-color-asides-border:\s*var\(--aether-mint\)/,
    );
    expect(styles).toMatch(
      /\.starlight-aside--caution[\s\S]*--sl-color-asides-border:\s*var\(--aether-amber\)/,
    );
    expect(styles).toMatch(
      /\.starlight-aside--danger[\s\S]*--sl-color-asides-border:\s*var\(--aether-danger\)/,
    );
  });

  it("keeps wide Markdown tables readable inside the content column", () => {
    const styles = read("src/styles/custom.css");

    expect(styles).toMatch(
      /\.sl-markdown-content table:not\(:where\(\.not-content \*\)\)\s*{[^}]*display:\s*block;[^}]*inline-size:\s*100%;[^}]*max-inline-size:\s*100%;[^}]*overflow-x:\s*auto;[^}]*overscroll-behavior-inline:\s*contain/,
    );
    expect(styles).toMatch(
      /\.sl-markdown-content table :is\(th, td\):not\(:where\(\.not-content \*\)\)\s*{[^}]*min-inline-size:\s*9rem;[^}]*padding:\s*0\.875rem 1rem;[^}]*text-align:\s*start;[^}]*vertical-align:\s*top;[^}]*word-break:\s*normal/,
    );
    expect(styles).toMatch(
      /\.sl-markdown-content\s+table\s+:is\(\s*th:first-child,\s*td:first-child,\s*th:last-child,\s*td:last-child\s*\):not\(:where\(\.not-content \*\)\)\s*{[^}]*padding-inline:\s*1rem/,
    );
    expect(styles).toMatch(
      /\.sl-markdown-content table th:not\(:where\(\.not-content \*\)\)\s*{[^}]*white-space:\s*nowrap/,
    );
  });

  it("makes direction labels more prominent than destination page names", () => {
    const styles = read("src/styles/custom.css");

    expect(styles).toMatch(
      /\.pagination-links a > span\s*{[\s\S]*font-size:\s*1\.125rem[\s\S]*font-weight:\s*700/,
    );
    expect(styles).toMatch(
      /\.pagination-links \.link-title\s*{[\s\S]*font-size:\s*0\.8125rem[\s\S]*font-weight:\s*500/,
    );
  });

  it("removes decorative motion when the operating system asks for it", () => {
    const styles = read("src/styles/custom.css");

    expect(styles).toMatch(
      /@media \(prefers-reduced-motion: reduce\)[\s\S]*animation-duration:\s*0\.01ms !important/,
    );
    expect(styles).toMatch(
      /@media \(prefers-reduced-motion: reduce\)[\s\S]*transition-duration:\s*0\.01ms !important/,
    );
    expect(styles).toMatch(
      /@media \(prefers-reduced-motion: reduce\)[\s\S]*\.pagination-links a:hover[\s\S]*transform:\s*none/,
    );
  });
});
