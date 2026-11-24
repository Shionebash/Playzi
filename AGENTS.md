# Playzi Workspace Rules

## Versioning Policy (Mandatory)
- Every code, UI, or configuration change to Playzi MUST increment the app version by `x.x.(x+1)` (patch bump), unless the user explicitly specifies a target version (e.g. `1.2.0` or `2.0.0`).
- Always use the version bump script so all manifests stay in sync (`Cargo.toml`, `crates/playzi-desktop/tauri.conf.json`, and `Cargo.lock`):
  - Default patch bump (`x.x.(x+1)`):
    - Windows: `powershell -ExecutionPolicy Bypass -File scripts/bump-version.ps1`
    - Linux/macOS: `sh scripts/bump-version.sh`
  - Specific version requested by user:
    - Windows: `powershell -ExecutionPolicy Bypass -File scripts/bump-version.ps1 <X.Y.Z>`
    - Linux/macOS: `sh scripts/bump-version.sh <X.Y.Z>`
- Do not bump the version more than once for a single logical user request.
- A Git `pre-commit` hook (`.githooks/pre-commit`) also enforces this automatically on manual commits (enable once with `git config core.hooksPath .githooks`; skips double-bumps if the version was already changed relative to `HEAD`).
