# Dot-source this file: makes sure `cmake` (needed to build libopus) is on PATH.
# CI runners have CMake already; on a dev PC it may come from `pip install --user cmake`, whose
# Scripts directory depends on the Python flavour (Microsoft Store Python keeps it below
# %LOCALAPPDATA%\Packages\...\LocalCache\local-packages).
if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
    $candidates = @()
    try {
        $base = (& python -c "import site;print(site.getuserbase())" 2>$null)
        if ($base) { $candidates += Get-ChildItem (Join-Path $base 'Python*\Scripts\cmake.exe') -ErrorAction SilentlyContinue }
    } catch { }
    $candidates += Get-ChildItem (Join-Path $env:APPDATA 'Python\Python*\Scripts\cmake.exe') -ErrorAction SilentlyContinue
    $candidates += Get-ChildItem (Join-Path $env:LOCALAPPDATA 'Packages\PythonSoftwareFoundation.Python.*\LocalCache\local-packages\Python*\Scripts\cmake.exe') -ErrorAction SilentlyContinue
    $found = $candidates | Select-Object -First 1
    if ($found) { $env:PATH = "$($found.DirectoryName);$env:PATH" }
    elseif (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
        Write-Warning "cmake not found: install it with 'winget install Kitware.CMake' or 'pip install --user cmake'."
    }
}
