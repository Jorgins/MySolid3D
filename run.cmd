@echo off
rem Запуск MySolid3D (release-сборка; при изменениях в коде cargo сам соберёт её заново).
rem CMake нужен только когда пересобирается ядро OpenCascade; CMake 4 не читает старые CMakeLists ядра без переменной ниже.
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%;C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin"
set "CMAKE_POLICY_VERSION_MINIMUM=3.5"
cd /d "%~dp0"
cargo run --release -- %*
