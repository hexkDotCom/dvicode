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

## Publishing to the VS Code Marketplace and Open VSX

Publishing is a manual step: run the `Release` workflow by hand with **publish** ticked. It
publishes the packages built in that run. It needs a one-time setup:

**VS Code Marketplace** (the extension store inside VS Code):

1. Sign in at <https://marketplace.visualstudio.com/manage> with a Microsoft account and create
   a publisher. Its id must equal `publisher` in `client/package.json` (`hexk`); if that id is
   taken, pick another and change `package.json`.
2. In Azure DevOps (<https://dev.azure.com>, same account), create a personal access token
   with organization **All accessible organizations** and scope **Marketplace > Manage**.
3. Add it to the GitHub repository as the secret `VSCE_PAT` (Settings > Secrets and variables
   > Actions).

**Open VSX** (the store used by VSCodium, Cursor, Windsurf, Gitpod and other VS Code forks):

1. Sign in at <https://open-vsx.org> with GitHub and accept the publisher agreement in your
   profile.
2. Create an access token in your profile and the namespace once:
   `npx ovsx create-namespace hexk -p <token>`.
3. Add the token to the GitHub repository as the secret `OVSX_PAT`.

## Before the first public release

- Publish from the public repository (or point `repository`, `bugs` and `homepage` in
  `client/package.json` at it): the Marketplace page links there, and links to a private
  repository do not work for users.
- Add a screenshot or two to the README; the Marketplace page shows the README.
- `"preview": true` in `client/package.json` shows a **Preview** badge; remove it when the
  extension is ready to be called stable.
- Versions on the Marketplace are permanent: every publish needs a new version number.
