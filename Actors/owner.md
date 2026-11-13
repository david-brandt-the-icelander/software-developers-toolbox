# Owner

**Kind:** person in a role
**Evidence:** `.github/workflows/release.yml` (`on`: running the workflow with a version bump, or pushing a `vX.Y.Z` tag, releases every tool; `homebrew`: the formula is pushed to the owner's tap with `HOMEBREW_TAP_TOKEN`), `macos/release/README.md` (`Homebrew formula (formula.sh, bottle.sh)`, `One-time signing setup`: the tap token, repository secrets and Apple accounts only the owner sets up)

## What they want from the system

- To publish a new version of every tool to the GitHub release, icelander.net/downloads and the zazzcat/homebrew-z tap: [Release use cases](../.github/.dev/use-case.json), `publish-a-release`.

## How they reach it

Running the release workflow on GitHub, or pushing tags to the repository, merging pull requests into main (which takes nothing else), the repository's settings and secrets on GitHub, and the tap repository zazzcat/homebrew-z.

## Not to be confused with

The [Developer](developer.md), who installs and runs the released tools. The repository does not tell them apart: the owner is a developer too, and the difference is only in who can run the release, push tags, merge and set secrets.
