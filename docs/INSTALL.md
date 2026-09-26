# Installing codegraph

codegraph is a single binary (`codegraph` / `codegraph.exe`) with no runtime
dependencies. Pick one install path; they all end with `codegraph` on your PATH.

| Platform | Release asset | Built on |
|---|---|---|
| Windows x64 | `codegraph-<ver>-windows-amd64.zip` | `x86_64-pc-windows-msvc` |
| Linux x64 | `codegraph-<ver>-linux-amd64.tar.gz` | `x86_64-unknown-linux-musl` (static-pie, no glibc needed) |
| Linux arm64 | `codegraph-<ver>-linux-arm64.tar.gz` | `aarch64-unknown-linux-gnu` (dynamic, needs glibc >= 2.34: Ubuntu 22.04+, Debian 12+, RHEL 9+) |
| macOS Apple silicon | `codegraph-<ver>-macos-arm64.tar.gz` | `aarch64-apple-darwin` |
| macOS Intel | `codegraph-<ver>-macos-amd64.tar.gz` | `x86_64-apple-darwin` |

Every release also carries `SHA256SUMS` (`sha256sum` format) covering all archives.
Archives are flat: the binary, `README.md` and `LICENSE` at the top level.

## While the repository is private

`crowbett/codegraph` is currently a **private** repo. Anonymous requests to
`github.com/.../releases/download/...`, `raw.githubusercontent.com` and the Git URL all
return 404. Every path below therefore needs GitHub auth until the repo is made public:

- scripts: set `GH_TOKEN` (or `GITHUB_TOKEN`) to a token with `contents: read` on the repo,
  e.g. `GH_TOKEN=$(gh auth token)`; the scripts then fetch through the GitHub API;
- fetching the scripts themselves: use `gh` instead of curl (shown below);
- cargo: an authenticated git (SSH key or credential helper);
- scoop / winget / Homebrew / npm: only work once the repo (and the tap / npm packages)
  are public, or with the per-tool token setup noted in each section.

Once the repo is public, drop the token bits; nothing else changes.

## Linux and macOS: install script

```sh
# public repo
curl -fsSL https://raw.githubusercontent.com/crowbett/codegraph/main/packaging/install.sh | sh

# private repo (now)
gh api -H 'Accept: application/vnd.github.raw' \
  repos/crowbett/codegraph/contents/packaging/install.sh \
  | GH_TOKEN=$(gh auth token) sh

# a specific version / another directory
... | sh -s -- --version 0.2.0 --dir ~/bin
```

The script detects OS (`linux`/`macos`) and arch (`amd64`/`arm64`, native arm64 even
under Rosetta), downloads the matching archive and `SHA256SUMS`, verifies the hash,
and installs `codegraph` into `~/.local/bin` (override: `--dir` or
`CODEGRAPH_INSTALL_DIR`). It never edits PATH or shell profiles; if the directory is not
on PATH it prints the `export PATH=...` line to add. Needs `curl` or `wget`, `tar`, and
`sha256sum` or `shasum`.

## Windows: install script

```powershell
# public repo
irm https://raw.githubusercontent.com/crowbett/codegraph/main/packaging/install.ps1 | iex

# private repo (now)
$env:GH_TOKEN = gh auth token
gh api -H "Accept: application/vnd.github.raw" repos/crowbett/codegraph/contents/packaging/install.ps1 | Out-String | iex

# with parameters
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/crowbett/codegraph/main/packaging/install.ps1))) -Version 0.2.0
```

Works in Windows PowerShell 5.1 and PowerShell 7. Installs `codegraph.exe` into
`%LOCALAPPDATA%\codegraph\bin` (override: `-Dir` or `$env:CODEGRAPH_INSTALL_DIR`) after
verifying `SHA256SUMS`, and prints the one command that adds the directory to your user
PATH if it is missing (it does not run it). Re-running upgrades in place, even while an
MCP server holds the old exe open (the old file is renamed aside first).

## Windows: scoop / winget

Manifests live in `packaging/scoop/codegraph.json` and `packaging/winget/`. They are not
in any public bucket or in winget-pkgs yet, and both download the release zip anonymously,
so they only work after the repo is public:

```powershell
scoop install https://raw.githubusercontent.com/crowbett/codegraph/main/packaging/scoop/codegraph.json
winget install --manifest packaging\winget   # from a clone; needs `winget settings --enable LocalManifestFiles`
```

## Homebrew (macOS / Linux)

`packaging/homebrew/codegraph.rb` is a ready formula (arm/intel x macOS/Linux, installs
the release tarball, `brew test` runs `codegraph version`). To use it, copy it to a tap
repo `crowbett/homebrew-tap` as `Formula/codegraph.rb`, then:

```sh
brew tap crowbett/tap
brew install codegraph
```

A private tap clones fine with `HOMEBREW_GITHUB_API_TOKEN` / git credentials, but the
formula's release-asset URLs are fetched anonymously by Homebrew's default download
strategy, so `brew install` fails until the codegraph repo is public. Use the install
script meanwhile.

## npm (any OS with Node 18+)

`packaging/npm/` holds an esbuild-style package set:

- `@crowbett/codegraph`: a plain-JS `codegraph` bin shim plus
  `optionalDependencies` on the platform packages;
- `@crowbett/codegraph-{win32-x64,linux-x64,linux-arm64,darwin-x64,darwin-arm64}`:
  one binary each, gated by `os`/`cpu` so npm installs only the matching one.

Once published: `npm i -g @crowbett/codegraph` or
`npx @crowbett/codegraph version`. `CODEGRAPH_BINARY=/path/to/codegraph`
overrides the binary the shim runs. Nothing is published yet (see "Cutting a release").
CI assembles the packages from each OS's build, installs them into a scratch project with
`npm install` and runs the shim, on Linux, macOS and Windows.

## cargo (build from source)

Needs Rust 1.85+ and a C compiler (tree-sitter grammars and bundled SQLite are C):

```sh
cargo install --git https://github.com/crowbett/codegraph --locked
# a tag
cargo install --git https://github.com/crowbett/codegraph --tag v0.2.0 --locked
```

While private, cargo must be able to authenticate to GitHub. Either use SSH
(`cargo install --git ssh://git@github.com/crowbett/codegraph.git --locked`
with `CARGO_NET_GIT_FETCH_WITH_CLI=true` so cargo uses your git/ssh-agent setup), or keep
the https URL with a git credential helper (`gh auth setup-git`) and
`CARGO_NET_GIT_FETCH_WITH_CLI=true`. The crate is `publish = false`; it is not on crates.io.

From a clone: `cargo install --path . --locked`.

## After installing

```sh
codegraph version
codegraph index ~/dev
codegraph mcp        # stdio MCP server; see README for client registration
```

## Cutting a release

1. Bump `version` in `Cargo.toml` (and `Cargo.lock` via `cargo build`), update
   CHANGELOG/README references, commit on `main`, and make sure CI is green on all three OSes.
2. Optional dry run: Actions -> release -> "Run workflow" (dry run checked), or
   `gh workflow run release.yml --ref main -f dry_run=true`. It builds all five archives and
   `SHA256SUMS` as workflow artifacts and never creates a release.
3. Tag and push: `git tag -s vX.Y.Z -m vX.Y.Z ; git push origin vX.Y.Z`. The tag must match
   `Cargo.toml` (the workflow fails otherwise). The workflow builds every target and creates
   a **draft** GitHub release with the archives, `SHA256SUMS` and generated notes.
4. Check the draft (download one archive, run `codegraph version`), then publish it.
5. Update the package manifests from the release's `SHA256SUMS`:
   - scoop `packaging/scoop/codegraph.json`: `version`, `url`, `hash` (windows zip);
   - winget `packaging/winget/*.yaml`: `PackageVersion` in all three, `InstallerUrl`,
     `InstallerSha256` (uppercase hex);
   - Homebrew `packaging/homebrew/codegraph.rb`: `version` and the four `sha256` lines,
     then copy it to the tap repo;
   - npm: download the release assets into a directory and stage the packages:
     ```sh
     gh release download vX.Y.Z -D dist/release
     node packaging/npm/build.mjs --archives dist/release --out dist/npm
     # run this on Linux/macOS: npm pack on Windows drops the unix binaries' exec bit
     # publish platform packages first, then the main package:
     for d in dist/npm/codegraph-*/; do (cd "$d" && npm publish); done
     (cd dist/npm/codegraph && npm publish)
     ```
     (`publishConfig.access` is `restricted`; switch to `public` when the repo goes public.)
   Commit the manifest updates as `chore(packaging): vX.Y.Z hashes`.
