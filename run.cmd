@echo off
rem Run MySolid3D. CMake (from Visual Studio Build Tools) is only needed when the OpenCascade kernel is (re)built.
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%;C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin"
cd /d "%~dp0"
cargo run -- %*
