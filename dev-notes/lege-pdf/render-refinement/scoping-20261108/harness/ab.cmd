@echo off
setlocal
rem Windows wrapper for ab.py: single-thread environment, pinned to the P-cores
rem (logical CPUs 0-11 on an i7-13700H, mask 0xFFF).
rem   ab.cmd <python.exe> [--pass N] ROUNDS A.exe B.exe -- <sessionbench args>
rem Use full .exe paths. Children inherit the affinity.
set RAYON_NUM_THREADS=1
set PDF_RENDERER_IMAGE_ROW_PAR=0
set PDF_RENDERER_JPX_CONCURRENCY=serial
set "PY=%~1"
set "ALL=%*"
for /f "tokens=1,* delims= " %%a in ("%ALL%") do set "REST=%%b"
start "" /affinity FFF /b /wait "%PY%" -I "%~dp0ab.py" %REST%
