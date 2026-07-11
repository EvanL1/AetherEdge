import { mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs'
import { dirname, extname, relative, resolve } from 'node:path'
import { spawnSync } from 'node:child_process'

const cwd = process.cwd()
const args = process.argv.slice(2)

const CHECK_STATUS = {
  PASS: 'PASS',
  WARN: 'WARN',
  FAIL: 'FAIL',
}

const coverageThresholds = {
  lines: 70,
  statements: 70,
  functions: 70,
  branches: 60,
}

const bundleThresholds = {
  // Element Plus JS tree-shaking 后约 ~750KB，800KB 为合理上限
  jsBytes: 800 * 1024,
  // Element Plus CSS 约 ~410KB（gzip 后 ~50KB），450KB 为合理上限
  cssBytes: 450 * 1024,
  // 项目含多张大体积设备 SVG/PNG（~21.5MB 实测），25MB 为合理上限
  distBytes: 25 * 1024 * 1024,
}

const auditPolicy = {
  failOn: ['critical', 'high'],
  warnOn: ['moderate'],
}

const auditRegistry = 'https://registry.npmjs.org'

const findArgValue = (flag) => {
  const index = args.indexOf(flag)
  return index === -1 ? null : (args[index + 1] ?? null)
}

const reportPath = resolve(cwd, findArgValue('--out') || 'reports/quality/quality-report.md')
const coverageSummaryPath = resolve(cwd, 'coverage/coverage-summary.json')
const distPath = resolve(cwd, 'dist')

const run = (command, commandArgs) => {
  const commandText = `${command} ${commandArgs.join(' ')}`.trim()
  const result =
    process.platform === 'win32'
      ? spawnSync(process.env.ComSpec || 'cmd.exe', ['/d', '/s', '/c', commandText], {
          cwd,
          stdio: 'pipe',
          shell: false,
          encoding: 'utf8',
          env: process.env,
        })
      : spawnSync(command, commandArgs, {
          cwd,
          stdio: 'pipe',
          shell: false,
          encoding: 'utf8',
          env: process.env,
        })

  const stdout = result.stdout ?? ''
  const stderr = result.stderr ?? ''
  const errorText = result.error ? `${result.error.name}: ${result.error.message}` : ''
  const combinedOutput = [stdout, stderr, errorText].filter(Boolean).join('\n').trim()

  if (stdout) process.stdout.write(stdout)
  if (stderr) process.stderr.write(stderr)
  if (errorText) process.stderr.write(`${errorText}\n`)

  return {
    ok: result.status === 0 && !result.error,
    status: result.status ?? 1,
    stdout,
    stderr: [stderr, errorText].filter(Boolean).join('\n').trim(),
    output: combinedOutput,
    command: commandText,
  }
}

const formatPercent = (value) => `${Number(value ?? 0).toFixed(2)}%`
const formatBytes = (value) => `${(Number(value ?? 0) / 1024).toFixed(2)} KB`

const toMarkdownCodeBlock = (content) => {
  const text = String(content ?? '').trim()
  return text ? `\`\`\`text\n${text}\n\`\`\`` : '```text\n<no output>\n```'
}

const readCoverageSummary = () => {
  const summary = JSON.parse(readFileSync(coverageSummaryPath, 'utf8'))
  const total = summary.total ?? {}

  return {
    lines: total.lines?.pct ?? 0,
    statements: total.statements?.pct ?? 0,
    functions: total.functions?.pct ?? 0,
    branches: total.branches?.pct ?? 0,
  }
}

const walkFiles = (dir) => {
  const entries = readdirSync(dir, { withFileTypes: true })
  const files = []

  for (const entry of entries) {
    const fullPath = resolve(dir, entry.name)
    if (entry.isDirectory()) {
      files.push(...walkFiles(fullPath))
    } else {
      files.push(fullPath)
    }
  }

  return files
}

const readDistSummary = () => {
  const files = walkFiles(distPath)
  const assets = []
  let totalBytes = 0
  let maxJsBytes = 0
  let maxCssBytes = 0
  let jsCount = 0
  let cssCount = 0
  let oversizedJsCount = 0
  let oversizedCssCount = 0

  for (const file of files) {
    const relPath = relative(distPath, file).replaceAll('\\', '/')
    if (relPath === 'stats.html' || relPath.endsWith('.gz') || relPath.endsWith('.br')) {
      continue
    }

    const size = statSync(file).size
    const extension = extname(file).toLowerCase()

    totalBytes += size
    assets.push({ path: relPath, size })

    if (extension === '.js') {
      jsCount += 1
      maxJsBytes = Math.max(maxJsBytes, size)
      if (size > bundleThresholds.jsBytes) oversizedJsCount += 1
    }

    if (extension === '.css') {
      cssCount += 1
      maxCssBytes = Math.max(maxCssBytes, size)
      if (size > bundleThresholds.cssBytes) oversizedCssCount += 1
    }
  }

  assets.sort((left, right) => right.size - left.size)

  return {
    totalBytes,
    maxJsBytes,
    maxCssBytes,
    jsCount,
    cssCount,
    oversizedJsCount,
    oversizedCssCount,
    topAssets: assets.slice(0, 5),
  }
}

const extractJsonObject = (text) => {
  const raw = String(text ?? '').trim()
  if (!raw) return null

  try {
    return JSON.parse(raw)
  } catch {}

  const lines = raw
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
  for (const line of lines) {
    try {
      return JSON.parse(line)
    } catch {}
  }

  const firstBrace = raw.indexOf('{')
  const lastBrace = raw.lastIndexOf('}')
  if (firstBrace === -1 || lastBrace === -1 || lastBrace < firstBrace) return null

  try {
    return JSON.parse(raw.slice(firstBrace, lastBrace + 1))
  } catch {
    return null
  }
}

const summarizeAudit = (commandResult) => {
  const parsed = extractJsonObject(commandResult.output)
  const summary = {
    source: commandResult.command,
    parsed,
    critical: 0,
    high: 0,
    moderate: 0,
    low: 0,
    parseError: '',
  }

  if (!parsed) {
    summary.parseError = '无法解析 npm audit 输出'
    return summary
  }

  if (parsed.error) {
    summary.parseError = '无法解析 npm audit 输出'
    return summary
  }

  const vulnerabilities = parsed.metadata?.vulnerabilities ?? parsed.vulnerabilities ?? {}
  summary.critical = Number(vulnerabilities.critical ?? 0)
  summary.high = Number(vulnerabilities.high ?? 0)
  summary.moderate = Number(vulnerabilities.moderate ?? 0)
  summary.low = Number(vulnerabilities.low ?? 0)

  return summary
}

const createCheck = ({
  name,
  commandResult,
  description,
  rule,
  status,
  basis,
  keyFindings = [],
}) => ({
  name,
  status,
  exitCode: commandResult.status,
  basis,
  rule,
  description,
  command: commandResult.command,
  output: commandResult.output,
  keyFindings,
})

const getStatusLabel = (status) => {
  if (status === CHECK_STATUS.PASS) return 'PASS 通过'
  if (status === CHECK_STATUS.WARN) return 'WARN 警告'
  return 'FAIL 失败'
}

const getOverallStatus = (checks) => {
  if (checks.some((check) => check.status === CHECK_STATUS.FAIL)) return CHECK_STATUS.FAIL
  if (checks.some((check) => check.status === CHECK_STATUS.WARN)) return CHECK_STATUS.WARN
  return CHECK_STATUS.PASS
}

const typeCheckResult = run('pnpm', ['type-check:only'])
const lintResult = run('pnpm', ['lint:check'])
const formatResult = run('pnpm', ['format:check'])
const unitTestResult = run('pnpm', ['test:run'])
const coverageResult = run('pnpm', ['test:coverage'])
const buildResult = run('pnpm', ['build:check'])
const auditResult = run('npm', [
  'audit',
  '--json',
  '--audit-level=moderate',
  `--registry=${auditRegistry}`,
])

let coverage = null
if (coverageResult.ok) {
  try {
    coverage = readCoverageSummary()
  } catch {
    coverage = null
  }
}

let distSummary = null
if (buildResult.ok) {
  try {
    distSummary = readDistSummary()
  } catch {
    distSummary = null
  }
}

const coverageFailures = coverage
  ? Object.entries(coverageThresholds).filter(
      ([metric, threshold]) => Number(coverage[metric] ?? 0) < threshold,
    )
  : []

const bundleWarnings = distSummary
  ? [
      distSummary.maxJsBytes > bundleThresholds.jsBytes ? '单个 JS 资源超出建议阈值' : null,
      distSummary.maxCssBytes > bundleThresholds.cssBytes ? '单个 CSS 资源超出建议阈值' : null,
      distSummary.totalBytes > bundleThresholds.distBytes ? 'dist 总体积超出建议阈值' : null,
    ].filter(Boolean)
  : []

const auditSummary = summarizeAudit(auditResult)
const auditHasFailSeverity = auditPolicy.failOn.some((level) => auditSummary[level] > 0)
const auditHasWarnSeverity = auditPolicy.warnOn.some((level) => auditSummary[level] > 0)

const checks = [
  createCheck({
    name: '类型检查',
    commandResult: typeCheckResult,
    description: '检查 TypeScript 与 Vue 类型错误。',
    rule: '执行 vue-tsc，命令失败则直接判定为失败。',
    status: typeCheckResult.ok ? CHECK_STATUS.PASS : CHECK_STATUS.FAIL,
    basis: typeCheckResult.ok ? '命令执行成功' : '命令执行失败',
  }),
  createCheck({
    name: '代码规范检查',
    commandResult: lintResult,
    description: '检查 ESLint 规则、潜在缺陷与不规范写法。',
    rule: '执行 ESLint，只要存在 error 或命令非 0 退出即判定为失败。',
    status: lintResult.ok ? CHECK_STATUS.PASS : CHECK_STATUS.FAIL,
    basis: lintResult.ok ? '命令执行成功' : '命令执行失败',
  }),
  createCheck({
    name: '格式检查',
    commandResult: formatResult,
    description: '检查代码是否符合 Prettier 格式要求。',
    rule: '执行 Prettier --check，格式不符合即判定为失败。',
    status: formatResult.ok ? CHECK_STATUS.PASS : CHECK_STATUS.FAIL,
    basis: formatResult.ok ? '命令执行成功' : '命令执行失败',
  }),
  createCheck({
    name: '单元测试',
    commandResult: unitTestResult,
    description: '执行 Vitest 单元测试。',
    rule: '测试命令非 0 退出则判定为失败。',
    status: unitTestResult.ok ? CHECK_STATUS.PASS : CHECK_STATUS.FAIL,
    basis: unitTestResult.ok ? '命令执行成功' : '命令执行失败',
  }),
  createCheck({
    name: '测试覆盖率',
    commandResult: coverageResult,
    description: '执行带覆盖率的测试，并评估核心覆盖指标。',
    rule: '命令失败则直接失败；若覆盖率低于阈值（lines 70%，statements 70%，functions 70%，branches 60%），则记为警告。',
    status: !coverageResult.ok
      ? CHECK_STATUS.FAIL
      : coverage && coverageFailures.length > 0
        ? CHECK_STATUS.WARN
        : CHECK_STATUS.PASS,
    basis: !coverageResult.ok
      ? '命令执行失败'
      : coverage && coverageFailures.length > 0
        ? `覆盖率未达到阈值，未达标项：${coverageFailures.map(([metric]) => metric).join('、')}`
        : '命令执行成功且覆盖率达到阈值',
    keyFindings: coverage
      ? [
          `行覆盖率：${formatPercent(coverage.lines)}（目标 ${coverageThresholds.lines}%）`,
          `语句覆盖率：${formatPercent(coverage.statements)}（目标 ${coverageThresholds.statements}%）`,
          `函数覆盖率：${formatPercent(coverage.functions)}（目标 ${coverageThresholds.functions}%）`,
          `分支覆盖率：${formatPercent(coverage.branches)}（目标 ${coverageThresholds.branches}%）`,
        ]
      : ['未能读取 coverage/coverage-summary.json'],
  }),
  createCheck({
    name: '构建验证',
    commandResult: buildResult,
    description: '验证项目是否可以成功构建。',
    rule: '构建命令非 0 退出则判定为失败。',
    status: buildResult.ok ? CHECK_STATUS.PASS : CHECK_STATUS.FAIL,
    basis: buildResult.ok ? '命令执行成功' : '命令执行失败',
  }),
  createCheck({
    name: '构建产物体积',
    commandResult: buildResult,
    description: '分析 dist 目录产物大小，识别过大的 JS/CSS 资源。',
    rule: '脚本能分析 dist 即退出码为 0；若单个 JS 超过 800 KB、单个 CSS 超过 450 KB 或 dist 总体积超过 25 MB，则记为警告。',
    status:
      !buildResult.ok || !distSummary
        ? CHECK_STATUS.FAIL
        : bundleWarnings.length > 0
          ? CHECK_STATUS.WARN
          : CHECK_STATUS.PASS,
    basis:
      !buildResult.ok || !distSummary
        ? '构建未成功完成，无法分析产物体积'
        : bundleWarnings.length > 0
          ? '构建已完成，但产物体积超出建议阈值'
          : '构建已完成，产物体积在建议阈值内',
    keyFindings: distSummary
      ? [
          `dist 总大小：${formatBytes(distSummary.totalBytes)}（建议不超过 ${formatBytes(bundleThresholds.distBytes)}）`,
          `JS 资源数量：${distSummary.jsCount}`,
          `CSS 资源数量：${distSummary.cssCount}`,
          `最大 JS 阈值：${formatBytes(bundleThresholds.jsBytes)}，超标数量：${distSummary.oversizedJsCount}`,
          `最大 CSS 阈值：${formatBytes(bundleThresholds.cssBytes)}，超标数量：${distSummary.oversizedCssCount}`,
          `体积最大的 5 个产物：${distSummary.topAssets
            .map((item) => `${item.path}：${formatBytes(item.size)}`)
            .join('；')}`,
        ]
      : ['未能读取 dist 目录信息'],
  }),
  createCheck({
    name: '依赖安全扫描',
    commandResult: auditResult,
    description: '通过 npm audit 检查高危与严重依赖漏洞。',
    rule: '严重或高危漏洞记为失败；中危漏洞记为警告。当前 failOn=critical, high，warnOn=moderate。',
    status: auditSummary.parseError
      ? CHECK_STATUS.FAIL
      : auditHasFailSeverity
        ? CHECK_STATUS.FAIL
        : auditHasWarnSeverity
          ? CHECK_STATUS.WARN
          : CHECK_STATUS.PASS,
    basis: auditSummary.parseError
      ? auditSummary.parseError
      : auditHasFailSeverity
        ? '检测到高危或严重漏洞'
        : auditHasWarnSeverity
          ? '检测到中危漏洞'
          : '未检测到高危、严重或中危漏洞',
    keyFindings: auditSummary.parseError
      ? ['未能解析 npm audit 输出，建议在 CI 日志中进一步检查原始输出。']
      : [
          `critical：${auditSummary.critical}`,
          `high：${auditSummary.high}`,
          `moderate：${auditSummary.moderate}`,
          `low：${auditSummary.low}`,
        ],
  }),
]

const overallStatus = getOverallStatus(checks)
const passCount = checks.filter((check) => check.status === CHECK_STATUS.PASS).length
const warnCount = checks.filter((check) => check.status === CHECK_STATUS.WARN).length
const failCount = checks.filter((check) => check.status === CHECK_STATUS.FAIL).length

const lines = []
lines.push(`生成时间：${new Date().toISOString()}`)
lines.push('')
lines.push(
  `总体结果：${overallStatus} ${overallStatus === CHECK_STATUS.PASS ? '通过' : overallStatus === CHECK_STATUS.WARN ? '警告' : '失败'}`,
)
lines.push('')
lines.push(`通过：${passCount}`)
lines.push('')
lines.push(`警告：${warnCount}`)
lines.push('')
lines.push(`失败：${failCount}`)
lines.push('')
lines.push('结果解读')
lines.push('')
lines.push('- 退出码 表示命令本身是否执行成功：0 为成功，非 0 为失败。')
lines.push('- 判定依据 表示这个检查为什么被判定为通过、警告或失败。')
lines.push('- 规则/阈值 表示这个检查使用的质量判断标准。')
lines.push('')
lines.push('失败项摘要')
lines.push('')

const failedChecks = checks.filter((check) => check.status === CHECK_STATUS.FAIL)
if (failedChecks.length === 0) {
  lines.push('- 无')
} else {
  for (const check of failedChecks) {
    lines.push(`- ${check.name}：${check.basis}`)
  }
}

lines.push('')
lines.push('警告项摘要')
lines.push('')

const warnedChecks = checks.filter((check) => check.status === CHECK_STATUS.WARN)
if (warnedChecks.length === 0) {
  lines.push('- 无')
} else {
  for (const check of warnedChecks) {
    lines.push(`- ${check.name}：${check.basis}`)
  }
}

lines.push('')
lines.push('检查总览')
lines.push('')
lines.push('| 检查项 | 结果 | 退出码 | 判定依据 | 规则/阈值 |')
lines.push('| --- | --- | --- | --- | --- |')
for (const check of checks) {
  lines.push(
    `| ${check.name} | ${getStatusLabel(check.status)} | ${check.exitCode} | ${check.basis} | ${check.rule} |`,
  )
}

lines.push('')
lines.push('当前阈值配置')
lines.push('')
lines.push(
  `- 覆盖率：lines ${coverageThresholds.lines}%，statements ${coverageThresholds.statements}%，functions ${coverageThresholds.functions}%，branches ${coverageThresholds.branches}%`,
)
lines.push(
  `- 产物体积：JS ${Math.round(bundleThresholds.jsBytes / 1024)} KB，CSS ${Math.round(bundleThresholds.cssBytes / 1024)} KB，dist 总体积 ${Math.round(bundleThresholds.distBytes / 1024 / 1024)} MB`,
)
lines.push(
  `- 安全扫描：failOn=${auditPolicy.failOn.join(', ')}，warnOn=${auditPolicy.warnOn.join(', ')}`,
)
lines.push('')

for (const check of checks) {
  lines.push(check.name)
  lines.push('')
  lines.push(`- 结果：${getStatusLabel(check.status)}`)
  lines.push(`- 退出码：${check.exitCode}`)
  lines.push(`- 判定依据：${check.basis}`)
  lines.push(`- 检查说明：${check.description}`)
  lines.push(`- 规则/阈值：${check.rule}`)

  if (check.keyFindings.length > 0) {
    lines.push('')
    lines.push('关键结论：')
    lines.push('')
    for (const finding of check.keyFindings) {
      lines.push(`- ${finding}`)
    }
  }

  lines.push('')
  lines.push(`命令：\`${check.command}\``)
  lines.push('')
  lines.push(toMarkdownCodeBlock(check.output))
  lines.push('')
}

mkdirSync(dirname(reportPath), { recursive: true })
writeFileSync(reportPath, `\uFEFF${lines.join('\n')}`, 'utf8')

console.log(`Quality report written to ${reportPath}`)

if (overallStatus === CHECK_STATUS.FAIL) {
  process.exit(1)
}
