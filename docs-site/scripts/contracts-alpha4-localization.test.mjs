import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const docsSiteRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
);
const contractsRoot = "locales/zh-CN/aethercontracts";

function read(relativePath) {
  return readFileSync(
    path.join(docsSiteRoot, contractsRoot, relativePath),
    "utf8",
  );
}

const alpha4Pages = [
  "index.md",
  "getting-started.md",
  "compatibility.md",
  "conformance.md",
  "MIGRATION.md",
  "spec/foundation.md",
  "spec/cloudlink-v1alpha1.md",
  "spec/distribution-v1alpha1.md",
  "spec/tck-v1alpha1.md",
  "spec/thing-model-v1alpha1.md",
  "spec/integration-v1alpha1.md",
  "spec/integration-control-v1alpha1.md",
];

describe("AetherContracts Chinese release status", () => {
  it("separates the published alpha.3 release from the alpha.4 development target", () => {
    for (const page of alpha4Pages) {
      const content = read(page);
      expect(content, page).toContain("0.1.0-alpha.4");
      expect(content, page).toContain("v0.1.0-alpha.3");
      expect(content, page).toContain("尚未发布");
    }

    expect(read("MIGRATION.md")).toContain("v0.1.0-alpha.3` 已经不可变");
    expect(alpha4Pages.map(read).join("\n")).not.toMatch(
      /(?:最新发布版本|当前发布版本|当前版本)是 `?v?0\.1\.0-alpha\.4/,
    );
  });

  it("keeps the English tagged contract authoritative and the release experimental", () => {
    const pages = alpha4Pages.map(read).join("\n");

    expect(pages).toContain("英文规范");
    expect(pages).toContain("仍处于实验阶段");
    expect(pages).toContain("不是生产");
    expect(pages).toContain("默认关闭");
  });

  it("preserves the Integration and Integration Control safety boundary", () => {
    const integration = read("spec/integration-v1alpha1.md");
    const control = read("spec/integration-control-v1alpha1.md");

    expect(integration).toContain("只读");
    expect(integration).toContain("提供方凭据");
    expect(control).toContain("device.power.set.v1");
    expect(control).toContain("提供方接受");
    expect(control).toContain("不能证明物理设备已经完成动作");
  });
});
