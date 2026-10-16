# Desk Bridge UI (Tauri 2 + React 19)

```
npm install
npm run dev            # Vite on http://127.0.0.1:1420, mock agent (browser): open /?mock
npm run build          # tsc + vite build -> dist/
npm run icons          # regenerate src-tauri/icons from src/brand/app-icon/variant-a.svg (npm run icons -- b|c)
npm run tauri dev      # real app (needs Rust, WebView2); talks to \\.\pipe\deskbridge-ui, starts deskbridge-agent.exe next to the exe
cargo check -p desk-bridge-app
```

Mock scenarios (query string, see `src/api/mock.ts`): `?mock&onboarding` (pairing code `123456`), `&role=laptop`,
`&state=disconnected`, `&offline`, `&focus=unavailable`, `&mic=live`, `&update`, `&theme=dark`, `&lang=en`, `&quiet`.
Extra windows in the browser: `/#/osd?demo`, `/#/flyout`. Deep links: `/#/audio`, `/#/focus`, ...

Updater: the `tauri-plugin-updater` dependency is currently removed from `src-tauri/Cargo.toml` (its zip dependency is yanked on
crates.io). The config in `tauri.conf.json` keeps the endpoint and a placeholder pubkey (TODO: generate with
`npm run tauri signer generate`). Re-enable by restoring the dependency and the `updater` feature (see Cargo.toml).
