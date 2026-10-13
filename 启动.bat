@echo off
chcp 65001 >nul
title 个人工作台
cd /d "%~dp0"

echo.
echo   正在启动个人工作台...
echo   关掉这个窗口就会停止运行。
echo.

start "" http://localhost:8787
node server.js

echo.
echo   服务已停止。按任意键关闭。
pause >nul
