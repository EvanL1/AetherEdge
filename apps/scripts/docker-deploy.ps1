<#
.SYNOPSIS
    AetherEMS 前端 Docker 部署脚本（Windows PowerShell）

.DESCRIPTION
    支持三种模式：
      local  （默认）本地单平台构建并运行
      export         构建 ARM64 镜像并导出为 .tar 文件
      push           构建 amd64+arm64 并推送到镜像仓库

.PARAMETER Mode
    local | export | push（默认 local）

.PARAMETER Tag
    镜像标签（默认 latest）

.PARAMETER Platform
    目标平台（默认根据 Mode 自动选择）

.PARAMETER Registry
    仓库地址前缀，如 docker.io/myuser 或 192.168.1.10:5000（push 模式必填）

.PARAMETER RemoteHost
    export 模式下自动 scp 并部署到远程主机，如 root@192.168.30.21

.PARAMETER NoCache
    完整重建（不使用缓存）

.EXAMPLE
    .\scripts\docker-deploy.ps1                                                      # 本地运行
    .\scripts\docker-deploy.ps1 -Mode export                                         # 导出 arm64 tar
    .\scripts\docker-deploy.ps1 -Mode export -RemoteHost root@192.168.30.21          # 导出并远程部署
    .\scripts\docker-deploy.ps1 -Mode push -Registry docker.io/myuser                # 多架构推送
    .\scripts\docker-deploy.ps1 -Tag v1.2.3 -NoCache                                 # 指定版本完整构建
#>

param(
    [ValidateSet("local", "export", "push")]
    [string]$Mode          = "local",
    [string]$Tag           = "latest",
    [string]$Platform      = "",
    [string]$Registry      = "",
    [string]$RemoteHost    = "",
    [string]$ContainerName = "aether-apps",
    [int]$Port             = 8080,
    [switch]$NoCache
)

[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
[Console]::InputEncoding  = [System.Text.Encoding]::UTF8
$OutputEncoding           = [System.Text.Encoding]::UTF8

$BuilderName = "aether-multiarch-builder"
$AppsDir     = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$ExportTar   = Join-Path (Get-Location) "${ContainerName}-arm64-${Tag}.tar"

# ── 根据模式自动选择平台 ───────────────────────────────────────────────────────
if (-not $Platform) {
    switch ($Mode) {
        "local"  { $Platform = "linux/amd64" }
        "export" { $Platform = "linux/arm64" }
        "push"   { $Platform = "linux/amd64,linux/arm64" }
    }
}

# ── 镜像全名 ──────────────────────────────────────────────────────────────────
$ImageFull = if ($Registry) { "${Registry}/${ContainerName}:${Tag}" } else { "${ContainerName}:${Tag}" }

# ── 颜色输出辅助函数 ───────────────────────────────────────────────────────────
function Write-Step($n, $msg) { Write-Host "`n[$n] $msg" -ForegroundColor Yellow }
function Write-OK($msg)        { Write-Host "  OK $msg"  -ForegroundColor Green  }
function Write-Info($msg)      { Write-Host "  -> $msg"  -ForegroundColor Gray   }
function Write-Fail($msg)      { Write-Host "[ERROR] $msg" -ForegroundColor Red; exit 1 }

Write-Host ""
Write-Host "================================================" -ForegroundColor Cyan
Write-Host "   AetherEMS Frontend Docker Deploy"            -ForegroundColor Cyan
Write-Host "   Mode: $Mode    Platform: $Platform"           -ForegroundColor Cyan
Write-Host "================================================" -ForegroundColor Cyan

# ── Step 0：检查环境 ───────────────────────────────────────────────────────────
Write-Step "0/5" "Checking environment"

docker version 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) { Write-Fail "Docker is not running. Please start Docker Desktop." }
Write-OK "Docker is ready"

docker buildx version 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) { Write-Fail "docker buildx not found. Please upgrade Docker to 20.10+." }
Write-OK "docker buildx is ready"

if (-not (Test-Path "$AppsDir\Dockerfile")) {
    Write-Fail "Dockerfile not found at: $AppsDir"
}

# ── Step 1：准备 Builder ───────────────────────────────────────────────────────
Write-Step "1/5" "Preparing builder"

if ($Mode -eq "local") {
    Write-Info "Local mode: using default builder (no cross-platform driver needed)"
} else {
    $builderExists = docker buildx ls 2>&1 | Select-String $BuilderName
    if (-not $builderExists) {
        Write-Info "Creating new builder: $BuilderName (docker-container driver)"
        docker buildx create --name $BuilderName --driver docker-container --driver-opt "network=host" --use 2>&1 | Out-Null
        docker buildx inspect --bootstrap 2>&1 | Out-Null
    } else {
        docker buildx use $BuilderName 2>&1 | Out-Null
        Write-Info "Reusing existing builder: $BuilderName"
    }
    Write-OK "Builder ready: $BuilderName"
}

# ── Step 2：清理旧容器（仅本地模式） ────────────────────────────────────────────
Write-Step "2/5" "Cleanup"

if ($Mode -eq "local") {
    $existing = docker ps -a --filter "name=$ContainerName" --format "{{.ID}}" 2>$null
    if ($existing) {
        Write-Info "Removing old container: $existing"
        docker stop $existing 2>&1 | Out-Null
        docker rm   $existing 2>&1 | Out-Null
        Write-OK "Removed: $ContainerName"
    } else {
        Write-Info "No existing container found, skipping"
    }
} else {
    Write-Info "Skipped (export/push mode does not need local cleanup)"
}

# ── Step 3：构建镜像 ──────────────────────────────────────────────────────────
Write-Step "3/5" "Building image"
Write-Info "Image:    $ImageFull"
Write-Info "Platform: $Platform"

$buildArgs = @("buildx", "build", "--platform", $Platform, "-t", $ImageFull)

if ($NoCache) {
    $buildArgs += "--no-cache"
    Write-Info "Using --no-cache (full rebuild)"
}

switch ($Mode) {
    "local" {
        $buildArgs += "--load"
        Write-Info "Output: load into local Docker daemon"
    }
    "export" {
        $buildArgs += @("--output", "type=docker,dest=$ExportTar")
        Write-Info "Output: $ExportTar"
    }
    "push" {
        if (-not $Registry) { Write-Fail "push mode requires -Registry parameter" }
        $buildArgs += "--push"
        Write-Info "Output: push to $Registry"
    }
}

$buildArgs += $AppsDir

# $ErrorActionPreference 保持 Continue，通过 $LASTEXITCODE 判断结果
& docker @buildArgs
if ($LASTEXITCODE -ne 0) { Write-Fail "Docker build failed (exit $LASTEXITCODE)" }
Write-OK "Build successful: $ImageFull"

# ── Step 4：部署 ──────────────────────────────────────────────────────────────
Write-Step "4/5" "Deploy"

switch ($Mode) {
    "local" {
        docker run -d --name $ContainerName --restart unless-stopped -p "${Port}:8080" $ImageFull 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) {
            Write-Host "[WARNING] Container failed to start, logs:" -ForegroundColor Yellow
            docker logs --tail 20 $ContainerName
            Write-Fail "Please check the logs above"
        }
        Write-OK "Container started: $ContainerName"
    }
    "export" {
        Write-OK "Image exported: $ExportTar"
        if ($RemoteHost) {
            Write-Info "Uploading to $RemoteHost..."
            $remoteTar = "/tmp/${ContainerName}-arm64.tar"
            scp $ExportTar "${RemoteHost}:${remoteTar}"
            if ($LASTEXITCODE -ne 0) { Write-Fail "scp failed" }
            Write-OK "Upload complete"

            Write-Info "Loading and starting on remote host..."
            $remoteCmd = @"
docker stop $ContainerName 2>/dev/null || true
docker rm   $ContainerName 2>/dev/null || true
docker load -i $remoteTar
docker run -d --name $ContainerName --restart unless-stopped -p ${Port}:8080 $ImageFull
echo 'Remote container started'
"@
            ssh $RemoteHost $remoteCmd
            if ($LASTEXITCODE -ne 0) { Write-Fail "Remote deploy failed" }
            Write-OK "Remote deploy complete"
        } else {
            Write-Host ""
            Write-Host "  To deploy on ARM64 server, run:" -ForegroundColor Gray
            Write-Host "    scp $ExportTar root@<ARM64_HOST>:/tmp/" -ForegroundColor Gray
            Write-Host "    ssh root@<ARM64_HOST> 'docker load -i /tmp/${ContainerName}-arm64-${Tag}.tar'" -ForegroundColor Gray
            Write-Host "    ssh root@<ARM64_HOST> 'docker run -d --name $ContainerName -p ${Port}:8080 $ImageFull'" -ForegroundColor Gray
        }
    }
    "push" {
        Write-OK "Multi-arch image pushed: $ImageFull"
        Write-Host ""
        Write-Host "  To run on ARM64 server:" -ForegroundColor Gray
        Write-Host "    docker pull $ImageFull" -ForegroundColor Gray
        Write-Host "    docker run -d --name $ContainerName -p ${Port}:8080 $ImageFull" -ForegroundColor Gray
    }
}

# ── Step 5：验证 ──────────────────────────────────────────────────────────────
Write-Step "5/5" "Done"

if ($Mode -eq "local") {
    Start-Sleep -Seconds 2
    $running = docker ps --filter "name=$ContainerName" --filter "status=running" --format "{{.Status}}" 2>$null
    if ($running) {
        Write-OK "Container status: $running"
        Write-Host ""
        Write-Host "================================================" -ForegroundColor Cyan
        Write-Host "   Deploy successful!  http://localhost:$Port"   -ForegroundColor Cyan
        Write-Host "================================================" -ForegroundColor Cyan
        Write-Host ""
        Write-Host "  View logs : docker logs -f $ContainerName" -ForegroundColor Gray
        Write-Host "  Enter     : docker exec -it $ContainerName sh" -ForegroundColor Gray
        Write-Host "  Stop      : docker stop $ContainerName" -ForegroundColor Gray
    } else {
        Write-Host "[WARNING] Container may not be running:" -ForegroundColor Yellow
        docker logs --tail 20 $ContainerName
        Write-Fail "Check logs above"
    }
} else {
    Write-Host ""
    Write-Host "================================================" -ForegroundColor Cyan
    Write-Host "   Build complete!  $ImageFull"                   -ForegroundColor Cyan
    Write-Host "   Platform: $Platform"                           -ForegroundColor Cyan
    Write-Host "================================================" -ForegroundColor Cyan
}
