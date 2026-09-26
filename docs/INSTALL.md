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

- scripts: set `GH_TOKEN` (or `GITHUB_TOKEN`) to a token with `contents: read` on the repo
  (see "Which token" below); the scripts then fetch through the GitHub API;
- fetching the scripts themselves: use `gh` instead of curl (shown below);
- cargo: an authenticated git (SSH key or credential helper);
- scoop / winget / Homebrew / npm: only work once the repo (and the tap / npm packages)
  are public, or with the per-tool token setup noted in each section.

Once the repo is public, drop the token bits; nothing else changes.

### Which token

Use a **fine-grained personal access token** made for this and nothing else: GitHub ->
Settings -> Developer settings -> Fine-grained tokens -> "Generate new token", resource
owner `crowbett`, "Only select repositories" = `codegraph`, repository
permission **Contents: Read-only** (Metadata: Read-only is added automatically), a short
expiry. That token can read releases and nothing else. `GH_TOKEN=$(gh auth token)` works
too, but it hands the script your full `gh` login (typically `repo` scope on every repo
you can reach), so prefer the fine-grained one on any machine you do not fully control.

How the scripts handle it:

- `install.sh` needs **curl** for the token path. The `Authorization` header goes into a
  mode-600 curl `--config` file inside the script's private temp dir (deleted on exit),
  never onto a command line where `ps` would show it to other local users. With only
  `wget` available it refuses to use a token (wget takes headers on its command line and
  re-sends them on redirects). All downloads are HTTPS-only, including redirects.
- `install.ps1` passes the token only as an HTTPS request header; Windows PowerShell 5.1
  and PowerShell 7 both drop it when GitHub redirects the download to its storage host.

Environment variables are still readable by your own user (and root), so run the
installer from a shell you trust and `unset GH_TOKEN` afterwards if you exported it.

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
on PATH it prints the `export PATH=...` line to add. Needs `curl` (or `wget` for public,
token-less downloads; GNU wget 1.15+ for `--https-only`), `tar`, and `sha256sum` or
`shasum`.

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
cargo install --git https://github.com/crowbett/codegraph --locked codegraph
# a tag
cargo install --git https://github.com/crowbett/codegraph --tag v0.2.0 --locked codegraph
```

The trailing package name `codegraph` is required: the repo also contains the `bench/`
crate and a Rust test fixture, and `cargo install --git` refuses to guess between them.

While private, cargo must authenticate to GitHub. The reliable way is SSH through your
git setup:

```sh
CARGO_NET_GIT_FETCH_WITH_CLI=true \
  cargo install --git ssh://git@github.com/crowbett/codegraph.git --locked codegraph
```

(PowerShell: `$env:CARGO_NET_GIT_FETCH_WITH_CLI = "true"` first.) The https URL also
works with `CARGO_NET_GIT_FETCH_WITH_CLI=true` and a git credential helper that already
holds a token (`gh auth setup-git`); with Git Credential Manager and no cached credential
it opens a login prompt, and in a non-interactive shell it just hangs. The crate is
`publish = false`; it is not on crates.io.

From a clone: `cargo install --path . --locked`.

## Verifying provenance

`SHA256SUMS` proves an archive was not corrupted in transit, but it is downloaded from the
same release as the archive, so it cannot prove *who built it*. Releases built by
`.github/workflows/release.yml` also carry a signed **SLSA build-provenance attestation**
for every archive (`actions/attest-build-provenance`), binding the file's digest to this
repository, the workflow file and the tag it ran on. Verify one with the GitHub CLI
(2.49+):

```sh
gh release download vX.Y.Z -R crowbett/codegraph -p 'codegraph-*-linux-amd64.tar.gz'
gh attestation verify codegraph-X.Y.Z-linux-amd64.tar.gz \
  -R crowbett/codegraph \
  --signer-workflow crowbett/codegraph/.github/workflows/release.yml
```

A pass prints the attestation's source repo, workflow and ref; any mismatch (a file not
built by that workflow in that repo) fails with a non-zero exit. `--signer-workflow`
also rejects artifacts attested by some other workflow in the repo. Offline:
`gh attestation download` the bundle once, then `gh attestation verify --bundle <file>`.

Caveats: GitHub only issues attestations for **public** repositories (private ones need
GitHub Enterprise Cloud). While this repo is private the release workflow skips the step
with a warning, so releases cut before it goes public (including v0.1.1) have no
attestation; verify those with `SHA256SUMS` only. The install scripts verify the
checksum, not the attestation; run `gh attestation verify` yourself when provenance matters.

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
   `Cargo.toml` (the workflow fails otherwise). The workflow builds every target without a
   build cache (hermetic), attests build provenance for each archive (public repo only),
   and creates a **draft** GitHub release with the archives, `SHA256SUMS` and generated notes.
4. Check the draft (download one archive, run `codegraph version`, and once the repo is
   public `gh attestation verify` it as in "Verifying provenance"), then publish it.
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
