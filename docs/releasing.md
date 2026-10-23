# Releasing DVICode

## What a release contains

The `Release` workflow (`.github/workflows/release.yml`) builds the engine for five
platforms and packages one extension per platform, plus a universal one with all of them:

| Package | Engine for local use | Built on |
|---|---|---|
| `dvicode-win32-x64-<v>.vsix` | Windows x64 (static C runtime) | Windows runner |
| `dvicode-linux-x64-<v>.vsix` | Linux x64 (static, any distribution) | Linux runner |
| `dvicode-linux-arm64-<v>.vsix` | Linux ARM64 (static) | Linux runner, cross-compiled |
| `dvicode-darwin-arm64-<v>.vsix` | macOS Apple Silicon | macOS runner |
| `dvicode-darwin-x64-<v>.vsix` | macOS Intel | macOS runner, cross-compiled |
| `dvicode-<v>.vsix` (universal) | all five of the above (about five times larger) | |

Every package also contains the static Linux x64 engine, which the extension installs in WSL
and on SSH hosts, and the license files (`LICENSE`, `NOTICE`, `THIRD_PARTY_NOTICES.md` for
exactly the engines it contains, and the Rust standard library notices).

The VS Code Marketplace and Open VSX serve each user the package for their platform when
there is one, and the universal package otherwise. On a platform without an engine (Windows
on ARM, for example), use WSL or SSH, or build the engine there and set
`dvicode.server.path`.

## Making a release

1. Run the pre-release checks ([development.md](development.md#before-a-release)); best on the
   universal package of a Release workflow run started by hand from `main`.
2. Update `version` in `client/package.json` and `server/Cargo.toml`, run `cargo update -w` in
   `server/` so that `server/Cargo.lock` has the new version too (the release builds use
   `--locked` and fail otherwise), and commit all three files.
3. Tag and push: `git tag vX.Y.Z && git push origin main vX.Y.Z` (the version of step 2).
4. The workflow checks that the tag matches the version, builds everything, runs the engine
   on the demo project on Windows, Linux and macOS, attests the packages' provenance, and
   creates a GitHub pre-release with the packages attached. Anyone can check that a package
   was built by this workflow from that tag:
   `gh attestation verify dvicode-X.Y.Z.vsix -R hexkDotCom/dvicode --signer-workflow
   hexkDotCom/dvicode/.github/workflows/release.yml --source-ref refs/tags/vX.Y.Z`
   (releases up to 0.3.1 have no attestations).
5. Try the packages, then edit the release on GitHub to mark it as the latest release.

To build packages without a release (for testing), run the workflow by hand from the
**Actions** tab; the packages are attached to the run as the `vsix` artifact.

## Publishing

The extension ID is `hexk.dvicode` (what users type in `code --install-extension`): the
publisher `hexk` on the VS Code Marketplace, which is also the namespace `hexk` on Open VSX,
followed by the extension name `dvicode`. `publisher` and `name` in `client/package.json`
must match them exactly, including case.

Published version numbers cannot be reused, even after a version is removed, so publish only
packages you have tried (step 5 of [Making a release](#making-a-release)).

### Open VSX

Open VSX is the store used by VSCodium, Cursor, Windsurf, Gitpod and other VS Code forks. The
`Release` workflow publishes to it with *trusted publishing*: Open VSX trusts the workflow
itself, so no access token is stored in GitHub. Run the workflow by hand from the **Actions**
tab, **from `main`** (the `open-vsx` environment accepts only that branch: a run from the tag
fails at once), with **tag** set to the release (`v0.3.2`), or
`gh workflow run release.yml -R hexkDotCom/dvicode --ref main -f tag=vX.Y.Z`. Nothing is built:
its `Publish to Open VSX` job waits for the reviewer's approval, downloads the release's six
packages, checks each against its provenance attestation (built by this workflow from that
tag), and publishes them. Re-running it skips versions that are already published; if a job
fails in a GitHub service step, start a new run rather than re-running the failed jobs.

One-time setup:

1. Sign in at <https://open-vsx.org> with GitHub, link an Eclipse Foundation account and sign
   the Publisher Agreement (Settings > Profile). Create the namespace `hexk` under Settings >
   Namespaces, then use its **Claim Ownership** link and wait for the request to be approved.
   Trusted publishing needs namespace *ownership*.
2. Publish the first version by hand with an access token. Trusted publishing can only be
   registered for an extension that already exists. Create a token under Settings > Access
   Tokens, then, with Node.js 22 or later, run this in the folder with the downloaded packages
   (it asks for the token once and saves it on this computer):

   ```bash
   for f in *.vsix; do npx --yes ovsx@1.2.0 publish "$f"; done
   ```

   Then remove the saved copy with `npx --yes ovsx@1.2.0 logout hexk` and delete the token on
   the website.
3. Under Settings > Trusted Publishers, register a publisher for the extension `hexk.dvicode`
   with provider **GitHub Actions**: the repository the workflow runs in (owner and name),
   workflow filename `release.yml`, environment `open-vsx`.
4. In the GitHub repository, create the environment `open-vsx` (Settings > Environments) and
   add yourself as a required reviewer, so every publish waits for your approval. Required
   reviewers need a public repository or a paid GitHub plan.

Anyone who can change `.github/workflows/release.yml` or run it in the `open-vsx` environment
can publish, so protect the branch and keep the reviewer rule.

### VS Code Marketplace

The Marketplace is the extension store inside VS Code. Its packages are uploaded by hand on the
publisher page, <https://marketplace.visualstudio.com/manage/publishers/hexk>:

1. Download the `vsix` artifact of the workflow run (or the files attached to the GitHub
   release).
2. The first time, choose **New extension > Visual Studio Code** and upload a package. For
   later packages and versions, choose **Update** in the extension's **...** menu.
3. Upload the universal package, `dvicode-<v>.vsix`: it contains every engine, so this one
   file works on every platform. Optionally also upload the platform packages, which are
   smaller downloads; the Marketplace serves each user the package for their platform when
   there is one. Check the extension's page afterwards.

Publishing from the workflow needs Azure. Azure DevOps retires global personal access
tokens on December 1, 2026; Microsoft recommends publishing with a Microsoft Entra ID
identity instead: a managed identity in an Azure subscription, added as a Contributor of the
publisher. Microsoft's guide,
[Publishing extensions](https://code.visualstudio.com/api/working-with-extensions/publishing-extension),
shows this for Azure Pipelines; from GitHub Actions, give the identity a federated credential
for this repository, sign in with `azure/login`, and run
`npx vsce publish --azure-credential --packagePath <file>` for each package (vsce 2.26.1 or
later).

## Before the first public release

- Publish from the public repository (or point `repository`, `bugs` and `homepage` in
  `client/package.json` at it): the Marketplace page links there, and links to a private
  repository do not work for users.
- When the user interface changes, record the README animation and screenshots again
  ([development.md](development.md#readme-media)). Both READMEs show them; the Marketplace
  page (`client/README.md`) loads them from `main` on GitHub.
- `"preview": true` in `client/package.json` shows a **Preview** badge; remove it when the
  extension is ready to be called stable.
