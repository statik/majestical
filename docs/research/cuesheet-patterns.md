I have everything I need. Here is the structured report.

---

# cuesheet — CI/CD + hosting patterns

Repo: `/Users/emurphy/kindlyops/cuesheet` · remote `git@github.com:kindlyops/cuesheet.git` · single initial-squash history, currently at v0.1.2, one tag `v0.1.1`.

## 1. Workflows

Three files, all under `/Users/emurphy/kindlyops/cuesheet/.github/workflows/`. **Every action is SHA-pinned with a trailing version comment**, and **every checkout sets `persist-credentials: false`**. Permissions are scoped per job, never workflow-wide.

### `ci.yml` — 4 parallel jobs, all `ubuntu-latest`

```yaml
name: CI
on:
  push:
    branches: [main]
  pull_request:
env:
  CARGO_TERM_COLOR: always
permissions:
  contents: read
```

| job | what it does |
|---|---|
| `rust` | `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` |
| `wasm` | `cargo build -p cuesheet-wasm --target wasm32-unknown-unknown` (compile-only smoke test) |
| `app` | Tauri app: apt system deps, `npm ci`, `npm run check` (svelte-check), `npm test` (vitest), `npm run build`, then `cargo clippy --all-targets -- -D warnings` in `src-tauri` |
| `site` | `npm ci && npm run build` in `site/` — no wasm build, so PRs validate the site's graceful-degradation path |

The canonical pinned action set reused everywhere:

```yaml
- uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0  # v7.0.0
  with:
    persist-credentials: false
- uses: actions/setup-node@48b55a011bda9f5d6aeb4c2d9c7362e8dae4041e  # v6.4.0
  with:
    node-version: 22
    cache: npm
    cache-dependency-path: site/package-lock.json   # only for the site job
- uses: dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8  # stable
  with:
    components: rustfmt, clippy
- uses: Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32  # v2
  with:
    workspaces: src-tauri     # only where the nested workspace is built
```

Tauri's Linux system deps (needed only for the CI clippy pass, since releases are macOS/Windows):

```yaml
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev \
  librsvg2-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev
```

### `pages.yml` — build + deploy, path-filtered

```yaml
name: Deploy Pages site
on:
  push:
    branches: [main]
    paths:
      - 'site/**'
      - 'crates/**'
      - '.github/workflows/pages.yml'
  workflow_dispatch:
permissions: {}
concurrency:
  group: pages
  cancel-in-progress: false
```

`crates/**` is in the path filter because the site embeds the wasm build of the Rust core. The build job holds `contents: read` + `pages: write`; the deploy job holds `pages: write` + `id-token: write` and nothing else.

wasm-pack is installed by direct tarball extract (no action, no cargo-install wait):

```yaml
- name: Install wasm-pack
  run: |
    curl -sSfL https://github.com/rustwasm/wasm-pack/releases/download/v0.13.1/wasm-pack-v0.13.1-x86_64-unknown-linux-musl.tar.gz \
      | tar xz --strip-components=1 -C /usr/local/bin --wildcards '*/wasm-pack'
- name: Build browser engine (wasm)
  run: wasm-pack build crates/cuesheet-wasm --target web --release --out-dir ../../site/public/wasm
```

Deploy tail — note `enablement: true`, which turns Pages on for the repo from the workflow itself rather than via repo settings:

```yaml
      - uses: actions/configure-pages@45bfe0192ca1faeb007ade9deae92b16b8254a0d  # v6.0.0
        with:
          enablement: true
      - uses: actions/upload-pages-artifact@fc324d3547104276b827a68afc52ff2a11cc49c9  # v5.0.0
        with:
          path: site/dist
  deploy:
    needs: build
    runs-on: ubuntu-latest
    permissions:
      pages: write
      id-token: write
    environment:
      name: github-pages
      url: ${{ steps.deployment.outputs.page_url }}
    steps:
      - id: deployment
        uses: actions/deploy-pages@cd2ce8fcbc39b97be8ca5fce6e763baed58fa128  # v5.0.0
```

### `release.yml` — tag-driven, 2-platform matrix, draft release

```yaml
name: Release
on:
  push:
    tags: ["v*"]
  # Manual trigger: builds the version in tauri.conf.json and creates the
  # v<version> tag + draft release itself (tauri-action expands __VERSION__).
  workflow_dispatch:
permissions: {}

jobs:
  build:
    strategy:
      fail-fast: false
      matrix:
        include:
          - platform: macos-latest
            args: --target universal-apple-darwin
          - platform: windows-latest
            args: ""
    runs-on: ${{ matrix.platform }}
    permissions:
      contents: write
    env:
      TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
      TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
```

No Linux target. No caching at all in this workflow — deliberately removed so a poisoned cache can't contaminate published artifacts, with the one unavoidable `setup-node` case suppressed inline:

```yaml
# No cache: input below, so setup-node writes no cache that could poison
# this publishing build; the cache-poisoning finding is a false positive.
- uses: actions/setup-node@48b55...  # v6.4.0  # zizmor: ignore[cache-poisoning]
```

The single most reusable idea here is the **degrade-gracefully-without-secrets** pattern: the whole signing apparatus is present but each piece self-disables when its secret is absent, so a fresh fork/clone can cut a release on day one.

```yaml
- name: Configure updater artifacts
  shell: bash
  run: |
    if [ -z "$TAURI_SIGNING_PRIVATE_KEY" ]; then
      echo 'UPDATER_CONFIG=--config {"bundle":{"createUpdaterArtifacts":false}}' >> "$GITHUB_ENV"
      echo "No updater signing key configured: updater artifacts disabled for this build."
    fi
```

Same shape for the three signing paths. macOS cert import is gated on a computed env var (`HAS_APPLE_CERT: ${{ secrets.APPLE_CERTIFICATE != '' }}` — the trick for conditioning a step on secret presence, since `secrets` isn't allowed in `if:`), creates an ephemeral keychain with a random password in `$RUNNER_TEMP`, and a separate step exports the Apple env only when the identity exists — with a comment explaining exactly why:

```yaml
# Tauri treats a set-but-empty APPLE_SIGNING_IDENTITY as "sign with
# identity \"\"" and fails; only export the Apple env when secrets exist
# so unsigned builds fall back to ad-hoc signing.
```

Windows uses a pwsh `Import-PfxCertificate` step that writes `WIN_CERT_THUMBPRINT` to `$GITHUB_ENV`, then feeds it back through the tauri-action args. The build step:

```yaml
- name: Build and publish draft release
  uses: tauri-apps/tauri-action@84b9d35b5fc46c1e45415bdb6144030364f7ebc5  # v0.6.2
  env:
    GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
  with:
    tagName: v__VERSION__
    releaseName: "Cuesheet v__VERSION__"
    releaseBody: |
      See the assets below to download and install.
      Installers are unsigned until code-signing credentials are
      configured; macOS Gatekeeper and Windows SmartScreen may warn on
      first launch.
    releaseDraft: true
    prerelease: false
    args: >-
      ${{ matrix.args }}
      ${{ env.UPDATER_CONFIG }}
      ${{ env.WIN_CERT_THUMBPRINT != '' && format('--config {{"bundle":{{"windows":{{"certificateThumbprint":"{0}"}}}}}}', env.WIN_CERT_THUMBPRINT) || '' }}
```

A third job, `licenses` (`needs: build`, `ubuntu-latest`, `contents: write`), generates a third-party license bundle with cargo-about and attaches it to the draft by looking the release up via the version in `tauri.conf.json`:

```yaml
- run: cargo install cargo-about --locked --features cli
- run: cargo about generate --workspace about.hbs -o THIRD_PARTY_LICENSES.html
- name: Upload to the draft release
  env:
    GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
  run: |
    TAG="v$(python3 -c "import json;print(json.load(open('src-tauri/tauri.conf.json'))['version'])")"
    RID=$(gh api "repos/$GITHUB_REPOSITORY/releases" \
      --jq ".[] | select(.tag_name==\"$TAG\") | .id" | head -1)
    gh api --method POST -H "Content-Type: text/html" \
      "https://uploads.github.com/repos/$GITHUB_REPOSITORY/releases/$RID/assets?name=THIRD_PARTY_LICENSES.html" \
      --input THIRD_PARTY_LICENSES.html
```

**Secrets used** (names only): `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`, `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD`, plus the built-in `GITHUB_TOKEN`.

**zizmor / actionlint:** neither runs in CI. The only trace is the inline `# zizmor: ignore[cache-poisoning]` comment, so zizmor was clearly run locally against these files but was never wired into a workflow. That's a gap worth closing in the new repo. Similarly there is **no `dependabot.yml`**, no `.pre-commit-config.yaml`, and no prek config anywhere in the repo.

## 2. Website (`site/`)

**Not Astro or SvelteKit** — it is a hand-written `index.html` plus plain TypeScript modules, built by bare Vite. Total source is seven files:

```
/Users/emurphy/kindlyops/cuesheet/site/index.html
/Users/emurphy/kindlyops/cuesheet/site/vite.config.ts
/Users/emurphy/kindlyops/cuesheet/site/package.json
/Users/emurphy/kindlyops/cuesheet/site/src/{main,downloads,webapp,scene,easterEgg}.ts
/Users/emurphy/kindlyops/cuesheet/site/src/style.css
```

It is a **separate npm project** from the app root (its own `package.json` + `package-lock.json`), depending only on `three` and `html2canvas`, dev-depending on `vite` + `typescript`. Build is `tsc --noEmit && vite build` — type-check gates the bundle.

Hosting is **GitHub Pages, project-site path**, deployed via the artifact/`deploy-pages` flow above. There is **no CNAME file and no custom domain**, so the site lives at `kindlyops.github.io/cuesheet/`, which is why the Vite base is set:

```ts
// site/vite.config.ts
export default defineConfig({
  base: '/cuesheet/',
  build: { target: 'es2022', sourcemap: false },
});
```

Patterns worth reusing:

- **Download buttons resolve themselves at runtime.** `site/src/downloads.ts` fetches `https://api.github.com/repos/kindlyops/cuesheet/releases/latest`, picks the first `.dmg` and first `.exe`/`.msi` asset, and rewrites the anchors' `href` plus a `name · tag` caption. The HTML ships with the buttons already pointing at the releases page, so a failed fetch, a rate limit, or JS-off all degrade to a working link with no error path. This removes the entire class of "site says v0.1.1, releases say v0.1.2" drift.
- **Same Rust core, compiled to wasm, running in the page.** `site/src/webapp.ts` lazy-loads `${import.meta.env.BASE_URL}wasm/cuesheet_wasm.js` on first use, but `HEAD`-probes it first so a build without the wasm bundle shows "the browser engine is not included in this build" instead of throwing a module-resolution error. `site/public/wasm/` is gitignored and only ever populated by `just wasm` or the Pages workflow — which is exactly why the CI `site` job (which skips wasm) still passes.
- **Everything below the hero is lazy.** three.js scene inits inside `requestIdleCallback` after first paint, with a WebGL-availability check and a static fallback card; the easter egg module is a dynamic import.

## 3. Repo conventions

**Two separate Cargo workspaces, deliberately.** The root workspace (`/Users/emurphy/kindlyops/cuesheet/Cargo.toml`) holds `cuesheet-core`, `cuesheet-typst`, `cuesheet-cli`, `cuesheet-wasm`. `src-tauri/Cargo.toml` declares its own empty `[workspace]` to opt out, with the reason in a comment:

```toml
# Deliberately its own workspace so this crate stays out of the root
# workspace (Tauri's heavy GUI deps don't belong in headless CI builds).
[workspace]
```

That split is what lets the `rust` and `wasm` CI jobs run without any GTK/WebKit system packages. Both workspaces set `[profile.release] lto = true, codegen-units = 1`.

**No `[lints]` section in either Cargo.toml.** Lint strictness is enforced purely by the CI command line (`-D warnings`), not declaratively. If the new repo is meant to follow the global standard's clippy lint table, this repo is not a template for that part.

**`justfile`** is the single entry point, and the last recipe is the key one — `just ci` runs locally exactly what CI runs:

```make
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
setup:
    npm install
    cd site && npm install
wasm:
    wasm-pack build crates/cuesheet-wasm --target web --release --out-dir ../../site/public/wasm
site:
    cd site && npm run dev
ci: check test frontend-check site-build
```

Also present: `bless` (regenerate golden test fixture via `BLESS=1`), `pdf` (headless CLI run), `app`/`app-build` (tauri dev/build), `signing-doc` (rebuilds `docs/signing-setup.pdf` from Typst source using the project's own Typst adapter — the repo dogfoods its PDF engine for its own docs).

**`scripts/`** contains only `make-icon.mjs` plus source art (`icon-source.svg`/`.png`) — a one-shot icon generator, not part of CI.

**Versioning** is manual and four-way. `docs/RELEASING.md` says to keep root `Cargo.toml` `[workspace.package] version`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, and root `package.json` in sync, then `git tag v0.2.0 && git push origin v0.2.0`. **This has already drifted**: root `Cargo.toml` is still `0.1.0` while the other three are `0.1.2`. There is no check enforcing the sync — an obvious thing to automate in the new repo (a CI job comparing the four, or a single source of truth).

The release flow itself: tag push → matrix build → tauri-action publishes a **draft** release with `latest.json` → licenses job attaches the HTML bundle → **human clicks Publish**, which is the only manual step and the moment updater clients see the new version.

## 4. Tauri updater config

`/Users/emurphy/kindlyops/cuesheet/src-tauri/tauri.conf.json`:

```json
  "bundle": {
    "active": true,
    "targets": ["dmg", "app", "nsis", "msi"],
    "createUpdaterArtifacts": true,
    "category": "Productivity",
    "copyright": "© Kindly Ops, LLC. Apache-2.0."
  },
  "plugins": {
    "updater": {
      "endpoints": [
        "https://github.com/kindlyops/cuesheet/releases/latest/download/latest.json"
      ],
      "pubkey": ""
    }
  }
```

The endpoint is the zero-infrastructure pattern: `releases/latest/download/latest.json` is a stable redirect that always resolves to the newest **published** (non-draft) release's manifest, so GitHub Releases is the whole update server — no hosting, no CDN, no manifest to maintain. `latest.json` is generated by tauri-action, not hand-written.

**`pubkey` is empty**, meaning the updater is scaffolded but not yet armed — which is precisely why `release.yml` has the `UPDATER_CONFIG` escape hatch. Consequences worth carrying forward: as configured today, the release workflow disables updater artifacts, so no `latest.json` is produced. `docs/RELEASING.md` documents the one-time fix (`npx @tauri-apps/cli signer generate -w ~/.tauri/cuesheet.key`, add the two secrets, paste the public key into the config).

Related wiring: `src-tauri/Cargo.toml` gates the plugin on desktop targets —

```toml
[target.'cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))'.dependencies]
tauri-plugin-updater = "2"
```

— and `src-tauri/src/lib.rs:82` registers `tauri_plugin_updater::Builder::new().build()`, with `updater:default` in `src-tauri/capabilities/default.json`. Note that **nothing in the Svelte frontend (`src/`) actually calls the updater API** — no `check()`/`relaunch()` anywhere — so the plugin is installed and permitted but there is no update check in the UI yet, despite the site advertising "Auto-updates itself from GitHub Releases". That's a real gap, not just a template detail.

One more Tauri-side note that bit this repo and will bite the next one: a pinned-dependency hazard is documented inline in `src-tauri/Cargo.toml` —

```toml
# NOTE: Cargo.lock pins `time` to 0.3.44 (with serde_with 3.15 / plist 1.7)
# because time >= 0.3.47 currently breaks tauri-utils 2.9 with E0119
# conflicting-From-impl errors. Re-test before running `cargo update`.
```
