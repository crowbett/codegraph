# Installing codegraph

codegraph is a single binary (`codegraph` / `codegraph.exe`) with no runtime
dependencies.

## What works today

| Channel | Status |
|---|---|
| Windows x64 release zip | available: `codegraph-0.1.1-windows-amd64.zip` + `.sha256` on [v0.1.1](https://github.com/crowbett/codegraph/releases/tag/v0.1.1) |
| `cargo install --git` (any OS) | available: builds from source, see below |
| Linux / macOS archives | not shipped yet (built by the release workflow, not attached to v0.1.1) |
| `install.sh` / `install.ps1` | not usable with v0.1.1: they need the release's `SHA256SUMS`, which v0.1.1 lacks |
| scoop / winget | manifests in `packaging/` point at the v0.1.1 zip; not in a public bucket or winget-pkgs |
| Homebrew | formula in `packaging/homebrew/`; no tap repo yet |
| npm | packages in `packaging/npm/`; not published |

The install scripts, Homebrew and npm consume the full asset set that
`.github/workflows/release.yml` builds (five archives plus `SHA256SUMS`), so they start
working with the first release cut by that workflow:

| Platform | Release asset | Built on |
|---|---|---|
| Windows x64 | `codegraph-<ver>-windows-amd64.zip` | `x86_64-pc-windows-msvc` |
| Linux x64 | `codegraph-<ver>-linux-amd64.tar.gz` | `x86_64-unknown-linux-musl` (static-pie, no glibc needed) |
| Linux arm64 | `codegraph-<ver>-linux-arm64.tar.gz` | `aarch64-unknown-linux-gnu` (dynamic, needs glibc >= 2.34: Ubuntu 22.04+, Debian 12+, RHEL 9+) |
| macOS Apple silicon | `codegraph-<ver>-macos-arm64.tar.gz` | `aarch64-apple-darwin` |
| macOS Intel | `codegraph-<ver>-macos-amd64.tar.gz` | `x86_64-apple-darwin` |

Archives are flat: the binary, `README.md` and `LICENSE` at the top level.

## Windows x64: release zip

```powershell
gh release download v0.1.1 -R crowbett/codegraph -p 'codegraph-0.1.1-windows-amd64.zip*'
Get-FileHash codegraph-0.1.1-windows-amd64.zip -Algorithm SHA256   # compare with the .sha256 file
Expand-Archive codegraph-0.1.1-windows-amd64.zip -DestinationPath "$env:LOCALAPPDATA\codegraph\bin"
```

(Or download both files from the release page in a browser.) Then add
`%LOCALAPPDATA%\codegraph\bin` to your user PATH.

## cargo (build from source)

Needs Rust 1.85+ and a C compiler (tree-sitter grammars and bundled SQLite are C):

```sh
cargo install --git https://github.com/crowbett/codegraph --locked codegraph
# a tag
cargo install --git https://github.com/crowbett/codegraph --tag v0.1.1 --locked codegraph
```

The trailing package name `codegraph` is required: the repo also contains the `bench/`
crate and a Rust test fixture, and `cargo install --git` refuses to guess between them.
The crate is `publish = false`; it is not on crates.io.

From a clone: `cargo install --path . --locked`.

## Planned channels

Written but not published. The install scripts, Homebrew and npm cannot install v0.1.1
(see the table above). CI syntax-checks `install.sh` and smoke-tests the npm shim; the
other channels are not exercised in CI.

### Linux and macOS: install script

```sh
curl -fsSL https://raw.githubusercontent.com/crowbett/codegraph/main/packaging/install.sh | sh

# a specific version / another directory
... | sh -s -- --version 0.2.0 --dir ~/bin
```

The script detects OS (`linux`/`macos`) and arch (`amd64`/`arm64`, native arm64 even
under Rosetta), downloads the matching archive and `SHA256SUMS`, verifies the hash,
and installs `codegraph` into `~/.local/bin` (override: `--dir` or
`CODEGRAPH_INSTALL_DIR`). It never edits PATH or shell profiles; if the directory is not
on PATH it prints the `export PATH=...` line to add. Needs `curl` (or GNU wget 1.15+ for
`--https-only`), `tar`, and `sha256sum` or `shasum`.

### Windows: install script

```powershell
irm https://raw.githubusercontent.com/crowbett/codegraph/main/packaging/install.ps1 | iex

# with parameters
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/crowbett/codegraph/main/packaging/install.ps1))) -Version 0.2.0
```

Works in Windows PowerShell 5.1 and PowerShell 7. Installs `codegraph.exe` into
`%LOCALAPPDATA%\codegraph\bin` (override: `-Dir` or `$env:CODEGRAPH_INSTALL_DIR`) after
verifying `SHA256SUMS`, and prints the one command that adds the directory to your user
PATH if it is missing (it does not run it). Re-running upgrades in place, even while an
MCP server holds the old exe open (the old file is renamed aside first).

Both scripts also accept `GH_TOKEN` / `GITHUB_TOKEN` and then download through the GitHub
API; a public install does not need it. `install.sh` takes the token path only with curl
and passes the header through a mode-600 curl config file, never on a command line.

### Windows: scoop / winget

Manifests live in `packaging/scoop/codegraph.json` and `packaging/winget/`. They are not
in any public bucket or in winget-pkgs yet; from the repo they can be used directly:

```powershell
scoop install https://raw.githubusercontent.com/crowbett/codegraph/main/packaging/scoop/codegraph.json
winget install --manifest packaging\winget   # from a clone; needs `winget settings --enable LocalManifestFiles`
```

### Homebrew (macOS / Linux)

`packaging/homebrew/codegraph.rb` is a ready formula (arm/intel x macOS/Linux, installs
the release tarball, `brew test` runs `codegraph version`). Its `sha256` lines are
placeholders until a release ships the tarballs. Once it is copied to a tap repo
`crowbett/homebrew-tap` as `Formula/codegraph.rb`:

```sh
brew tap crowbett/tap
brew install codegraph
```

### npm (any OS with Node 18+)

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

Caveats: GitHub only issues attestations for public repositories, and v0.1.1 was published
before this repo went public, so it has no attestation (and no `SHA256SUMS`); verify its zip
against the attached `.sha256` only. The install scripts verify the checksum, not the
attestation; run `gh attestation verify` yourself when provenance matters.

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
4. Check the draft (download one archive, run `codegraph version`, and
   `gh attestation verify` it as in "Verifying provenance"), then publish it.
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
     (`publishConfig.access` is still `restricted`; switch it to `public` before the first publish.)
   Commit the manifest updates as `chore(packaging): vX.Y.Z hashes`.
