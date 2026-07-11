# CloudEMS 测试规范与内容复用指南

> 快照时间：2026-04-14
>
> 本文基于当前仓库中的实际配置、测试代码、Git Hooks、CI 工作流和覆盖率结果整理，用于在其他项目中复用同类测试体系。

## 1. 当前项目采用的测试体系

### 1.1 测试技术栈

- 单元测试框架：`Vitest`
- 组件/组合式测试：`@vue/test-utils`
- DOM 环境：`jsdom`
- 覆盖率方案：`@vitest/coverage-v8`
- 测试相关 ESLint 规则：`@vitest/eslint-plugin`
- 接口场景测试：`Apifox CLI`（GitHub Actions 中执行）

### 1.2 执行命令

```bash
pnpm test:unit        # 启动 Vitest watch
pnpm test:run         # 单次执行测试
pnpm test:coverage    # 执行测试并生成覆盖率报告
pnpm quality:report   # 执行完整质量报告
pnpm quality:check    # 执行质量报告并输出到 reports/quality/quality-report.md
```

### 1.3 当前实际运行结果

- 测试文件数：`10`
- 测试用例数：`51`
- 当前结果：`10/10` 文件通过，`51/51` 用例通过
- 当前总覆盖率：
  - `lines`: `8.71%`
  - `statements`: `8.71%`
  - `functions`: `77.02%`
  - `branches`: `81.46%`

## 2. 目录、命名与编译边界

### 2.1 目录规范

- 测试文件统一放在业务代码同级的 `__tests__` 目录中。
- 当前示例：
  - `src/utils/__tests__/*.test.ts`
  - `src/stores/__tests__/*.test.ts`
  - `src/router/__tests__/*.test.ts`
  - `src/composables/__tests__/*.test.ts`

### 2.2 命名规范

- 文件命名统一使用 `*.test.ts`
- `describe` 标题优先写被测文件路径，如：
  - `composables/useTableData.ts`
  - `router/injector.ts`
  - `stores/user.ts`
  - `utils/timePicker.ts`

### 2.3 TypeScript 编译边界

- `tsconfig.app.json` 排除测试文件：`src/**/__tests__/*`
- `tsconfig.vitest.json` 单独纳入测试文件：`src/**/__tests__/*`
- 根 `tsconfig.json` 同时引用业务编译配置与测试编译配置

这套做法适合复用到其他项目，优点是：

- 业务构建不被测试代码污染
- 测试环境可以单独声明 `node`、`jsdom` 类型
- IDE 和 CI 都能清晰区分业务代码与测试代码

## 3. Vitest 配置规范

当前项目中的关键约束如下：

- `environment: 'jsdom'`
- 继承 `vite.config.ts`
- 排除目录：`e2e/**`
- 覆盖率目录：`coverage/`
- 覆盖率报告格式：
  - `text`
  - `json`
  - `json-summary`
  - `html`
  - `lcov`
- 覆盖率统计范围：`src/**/*.{js,ts,vue}`
- 覆盖率排除项：
  - `src/**/*.d.ts`
  - `src/**/*.test.{js,ts}`
  - `src/**/*.spec.{js,ts}`
  - `src/main.ts`
- CSS Modules 测试策略：`classNameStrategy: 'non-scoped'`

适合其他项目直接复用的原则：

- 前端单测优先使用 `jsdom`
- 测试配置与构建配置合并，避免 alias、插件行为不一致
- 覆盖率报告至少保留 `text + html + lcov + json-summary`
- 测试本身不纳入覆盖率

## 4. ESLint、Hooks 与质量门禁

### 4.1 ESLint 规则

- 测试目录 `src/**/__tests__/*` 启用 `@vitest/eslint-plugin` 推荐规则
- 全局忽略：
  - `dist`
  - `coverage`
  - `auto-imports.d.ts`
  - `components.d.ts`

### 4.2 Git Hooks

#### pre-commit

仅检查暂存区文件：

- 对 `ts/mts/tsx/js/mjs/cjs/vue` 执行 `eslint --max-warnings=0`
- 对 `ts/mts/tsx/js/mjs/cjs/vue/json/md/css/scss/html/yml/yaml` 执行 `prettier --check`

#### pre-push

- 执行完整质量报告：`pnpm quality:report -- --out .git/quality-reports/pre-push-report.md`

### 4.3 CI 质量门禁

GitHub Actions 中的 `quality-report.yml` 会执行：

1. `pnpm install --frozen-lockfile`
2. `pnpm quality:report -- --out quality-report.md`
3. 将报告写入 workflow summary
4. 上传 `quality-report.md` 和 `coverage/` 产物
5. 在 PR 中自动评论质量报告
6. 若质量报告失败，则 CI 失败

### 4.4 质量报告实际检查项

质量脚本 `scripts/quality-report.mjs` 当前会检查：

1. 类型检查
2. ESLint
3. Prettier 格式检查
4. Vitest 单元测试
5. 覆盖率阈值
6. 构建
7. 构建产物体积
8. 依赖安全扫描

### 4.5 当前阈值

- 覆盖率阈值：
  - `lines >= 70%`
  - `statements >= 70%`
  - `functions >= 70%`
  - `branches >= 60%`
- 构建产物阈值：
  - 单个 JS <= `500 KB`
  - 单个 CSS <= `250 KB`
  - `dist` 总体积 <= `15 MB`
- 依赖安全策略：
  - `critical/high` 直接失败
  - `moderate` 记为警告

## 5. 当前仓库已覆盖的测试内容

### 5.1 工具函数

#### `src/utils/common.ts`

- `removeEmpty`
  - 删除 `null`、空字符串、`undefined`
  - 保留 `0`、`false`、空数组
  - 非对象输入返回空对象
- `arrayToObjectByKey`
  - 按 key 建索引
  - 忽略空 key
  - 重复 key 保留最后一项
  - 非数组输入返回空对象

#### `src/utils/directives.ts`

- 权限指令挂载时的 DOM 保留/移除逻辑
- 仅按当前实现中的“首个角色”匹配
- 无父节点时不抛异常

#### `src/utils/request.ts`

- 模块加载时创建 axios 实例并注册拦截器
- `Request.get/post` 是否调用共享 service
- 文件上传是否使用 `multipart/form-data`
- 文件下载是否走 blob URL 流程

#### `src/utils/responsive.ts`

- 根字体大小计算
- resize 监听与防抖更新
- `px -> responsive px`
- `px -> rem`
- 当前缩放比例、字体大小获取

#### `src/utils/time.ts`

- 最近 `6h / 24h / 1week / 1month` 时间区间
- range type 到 helper 的映射
- 不支持的类型抛错
- 时间格式化

#### `src/utils/timePicker.ts`

- 起止时间变化联动清理
- 小时、分钟、秒级禁用规则
- 起止日期禁用规则
- 时间范围校验
- `HH:mm:ss` 格式化

### 5.2 Store

#### `src/stores/global.ts`

- 默认状态初始化
- UI 状态可独立修改
- store id 稳定性

#### `src/stores/user.ts`

- 登录成功路径
- 登录失败路径
- 获取用户信息成功路径
- token 刷新成功路径
- token 刷新失败后的登出、路由回退与状态清理
- 主动清理用户数据与主路由恢复

### 5.3 路由

#### `src/router/injector.ts`

- 侧边栏路由按角色过滤
- role 缺失时的兜底逻辑
- 动态路由注入
- 根路由重定向更新
- token 刷新失败时中断注入并登出
- 动态路由移除与状态重置

### 5.4 Composable

#### `src/composables/useTableData.ts`

- 组件挂载后自动拉取列表
- 查询参数清洗
- 搜索/筛选时重置到第一页
- 排序参数转换为接口字段
- delete/export 未开启时提示
- delete/export 开启后的请求与反馈行为
- 导出时携带最新过滤条件

## 6. 当前覆盖率现状与结论

### 6.1 覆盖率表现较好的模块

- `src/stores/global.ts`: `100%`
- `src/utils/common.ts`: `100%`
- `src/utils/directives.ts`: `100%`
- `src/utils/responsive.ts`: `100%`
- `src/utils/timePicker.ts`: `94.08%`
- `src/utils/time.ts`: `90%`
- `src/stores/user.ts`: `90.15%`
- `src/router/injector.ts`: `91.57%`

### 6.2 覆盖不足的区域

几乎未覆盖：

- `src/api/*`
- 大部分 `src/views/*`
- 大部分 `src/components/*`
- `src/utils/stomp.ts`
- `src/composables/useStomp.ts`
- `src/stores/station.ts`
- `src/router/guard.ts`

### 6.3 重要观察

- 当前“函数覆盖率”和“分支覆盖率”不低，但“行覆盖率/语句覆盖率”极低，说明测试集中在少量核心逻辑模块。
- 现有测试策略明显偏向：
  - 纯函数
  - store 行为
  - 路由注入逻辑
  - composable 行为
  - request 封装层
- 现阶段尚未形成系统性的页面级、组件级、接口级自动化测试矩阵。

## 7. 可直接复用到其他项目的测试规范

### 7.1 推荐保留的最低标准

1. 业务代码同级建立 `__tests__` 目录
2. 统一使用 `*.test.ts`
3. 前端单测统一走 `Vitest + jsdom`
4. 所有测试可通过 `pnpm test:run` 单次执行
5. 所有项目必须支持 `pnpm test:coverage`
6. 覆盖率报告至少输出 `html` 和 `lcov`
7. pre-commit 至少做 `eslint + prettier`
8. pre-push 至少跑一次完整质量检查
9. CI 必须产出覆盖率与质量报告
10. PR 必须能看到自动化测试结果

### 7.2 推荐测试优先级

优先补以下模块：

1. 纯函数和工具函数
2. store 的状态迁移和异常路径
3. 路由守卫、权限注入、登录态恢复
4. composable 的输入输出与副作用
5. 请求封装层、上传下载逻辑
6. 页面中的关键交互组件
7. 接口场景测试或端到端测试

### 7.3 推荐断言风格

- 一个用例只验证一个明确行为
- 优先断言“输入 -> 输出 / 状态变化 / 调用参数”
- 对副作用模块明确断言：
  - API 调用参数
  - 路由跳转
  - 本地存储变化
  - DOM 是否移除/保留
  - 提示消息是否触发

### 7.4 推荐 mock 规则

- 外部依赖全部 mock：
  - `axios`
  - `element-plus`
  - `vue-router`
  - `pinia store`
  - API 模块
- 每个 `beforeEach` 重置 mock
- 避免跨用例共享脏状态
- 对需要模块初始化副作用的文件，使用 `vi.resetModules()` 后再 `import()`

## 8. 可迁移模板

### 8.1 package.json scripts 模板

```json
{
  "scripts": {
    "test:unit": "vitest",
    "test:run": "vitest run --passWithNoTests",
    "test:coverage": "vitest run --coverage --passWithNoTests",
    "quality:report": "node scripts/quality-report.mjs"
  }
}
```

### 8.2 目录模板

```text
src/
  utils/
    xxx.ts
    __tests__/
      xxx.test.ts
  stores/
    xxx.ts
    __tests__/
      xxx.test.ts
  router/
    xxx.ts
    __tests__/
      xxx.test.ts
```

### 8.3 新项目落地建议

- 第一阶段先复制：
  - `vitest.config.ts`
  - `tsconfig.vitest.json`
  - 测试 scripts
  - `quality-report.mjs`
  - `.githooks/pre-commit`
  - `.githooks/pre-push`
- 第二阶段补齐：
  - 覆盖率阈值
  - CI workflow
  - PR 报告输出
- 第三阶段再引入：
  - Apifox / E2E / 接口回归测试

## 9. 本项目可继续补强的方向

如果继续完善 CloudEMS 自身测试体系，优先级建议如下：

1. 补 `router/guard.ts`
2. 补 `stores/station.ts`
3. 补 `utils/stomp.ts` 与 `composables/useStomp.ts`
4. 补 `api` 层调用约束测试
5. 为关键页面补组件测试
6. 将 Apifox DEV 流程扩展为稳定的多环境接口回归

## 10. 结论

CloudEMS 当前已经具备一套可复用的前端测试骨架：

- 有单测框架
- 有覆盖率
- 有 lint/format/type-check
- 有 Git Hooks
- 有 CI 质量报告
- 有接口场景测试入口

真正适合复用到其他项目的核心，不只是 `Vitest` 本身，而是下面这条完整链路：

`本地提交检查 -> 推送前质量报告 -> CI 统一校验 -> PR 可视化反馈 -> 覆盖率与接口场景并行演进`
