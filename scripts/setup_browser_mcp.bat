@echo off
setlocal enabledelayedexpansion
chcp 65001 >nul

echo ============================================
echo   安装 @playwright/mcp (Windows)
echo ============================================
echo.

where node >nul 2>nul
if errorlevel 1 (
    echo [错误] 未找到 node。请先安装 Node.js: https://nodejs.org/
    pause
    exit /b 1
)
where npm >nul 2>nul
if errorlevel 1 (
    echo [错误] 未找到 npm。请重新安装 Node.js（含 npm）。
    pause
    exit /b 1
)

set "MCP_DIR=%USERPROFILE%\.ligong-mcp"
echo [1/3] 安装目录: %MCP_DIR%
if not exist "%MCP_DIR%" mkdir "%MCP_DIR%"
cd /d "%MCP_DIR%"

if not exist package.json (
    echo   初始化 package.json
    call npm init -y >nul 2>&1
)

echo [2/3] 安装 @playwright/mcp ...
call npm install @playwright/mcp --no-audit --no-fund
if errorlevel 1 (
    echo [错误] npm install 失败。
    pause
    exit /b 1
)

set "CLI=%MCP_DIR%\node_modules\@playwright\mcp\cli.js"
if not exist "%CLI%" (
    echo [错误] 未找到 %CLI%
    pause
    exit /b 1
)
echo   cli.js: %CLI%

echo [3/3] 安装 Chromium ...
call npx --yes playwright install chromium
if errorlevel 1 (
    echo [警告] Chromium 安装失败，可稍后手动执行: npx playwright install chromium
) else (
    echo   Chromium 安装完成
)

echo.
echo ============================================
echo   安装完成
echo ============================================
echo 力工Studio 会自动在 %MCP_DIR% 下查找 @playwright/mcp。
echo 若仍提示找不到，可设置环境变量 LIGONG_MCP_CLI 指向 cli.js。
echo.
pause
