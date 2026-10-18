# Releasing DVICode

## What a release contains

The `Release` workflow (`.github/workflows/release.yml`) builds the engine for five
platforms and packages one extension per platform, plus a universal one:

| Package | Engine for local use | Built on |
|---|---|---|
| `dvicode-win32-x64-<v>.vsix` | Windows x64 (static C runtime) | Windows runner |
| `dvicode-linux-x64-<v>.vsix` | Linux x64 (static, any distribution) | Linux runner |
| `dvicode-linux-arm64-<v>.vsix` | Linux ARM64 (static) | Linux runner, cross-compiled |
| `dvicode-darwin-arm64-<v>.vsix` | macOS Apple Silicon | macOS runner |
| `dvicode-darwin-x64-<v>.vsix` | macOS Intel | macOS runner, cross-compiled |
| `dvicode-<v>.vsix` | none (WSL, SSH and other remote modes only) | |

Every package also contains the static Linux x64 engine, which the extension installs in WSL
and on SSH hosts, and the license files (`LICENSE`, `NOTICE`, `THIRD_PARTY_NOTICES.md` for
exactly the engines it contains, and the Rust standard library notices).

The VS Code Marketplace and Open VSX serve each user the package for their platform; the
universal package covers any other platform.

## Making a release

1. Update `version` in `client/package.json` (and `server/Cargo.toml` to match) and commit.
2. Tag and push: `git tag v0.2.0 && git push origin main v0.2.0`.
3. The workflow checks that the tag matches the version, builds everything, runs the engine
   on the demo project on Windows, Linux and macOS, and creates a GitHub pre-release with the
   packages attached.
4. Try the packages, then edit the release on GitHub to mark it as the latest release.

To build packages without a release (for testing), run the workflow by hand from the
**Actions** tab; the packages are attached to the run as the `vsix` artifact.

## Publishing

The extension is published under the id `hexk` in both stores: the publisher `hexk` on the
VS Code Marketplace and the namespace `hexk` on Open VSX. `publisher` in `client/package.json`
must match it exactly, including case.

Every store version is permanent, so publish only packages you have tried (step 4 of
[Making a release](#making-a-release)).

### Open VSX

Open VSX is the store used by VSCodium, Cursor, Windsurf, Gitpod and other VS Code forks. The
`Release` workflow publishes to it with *trusted publishing*: Open VSX trusts the workflow
itself, so no access token is stored in GitHub. Run the workflow by hand from the **Actions**
tab with **publish** ticked; its `Publish to Open VSX` job publishes the packages built in that
run. Re-running it skips versions that are already published.

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
3. Upload every platform package and the universal one. The Marketplace serves each user the
   package for their platform. Check the extension's page afterwards to confirm that all
   platforms are listed.

Publishing from the workflow needs a personal access token, which needs an Azure DevOps
organization (and so an Azure subscription). With one, create a token with organization **All
accessible organizations** and scope **Marketplace > Manage**, store it as the secret
`VSCE_PAT` of a GitHub environment with the same reviewer rule, and add a job in that
environment that runs `npx vsce publish --packagePath <file>` for each package.

## Before the first public release

- Publish from the public repository (or point `repository`, `bugs` and `homepage` in
  `client/package.json` at it): the Marketplace page links there, and links to a private
  repository do not work for users.
- Add a screenshot or two to the README; the Marketplace page shows the README.
- `"preview": true` in `client/package.json` shows a **Preview** badge; remove it when the
  extension is ready to be called stable.
- Versions on the Marketplace are permanent: every publish needs a new version number.
