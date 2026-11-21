# Owner

**Kind:** person in a role
**Evidence:** `release/README.md` (`Version numbers (version.sh, set-version.sh)`: running `release.sh` in continuous_integration_and_delivery with a version bump releases every tool; `Homebrew formula (formula.sh, bottle.sh)`: the formula is pushed to the owner's tap; `One-time signing setup`: the tap token, the release credentials and the Apple accounts only the owner sets up)

## What they want from the system

- To publish a new version of every tool to the GitHub release, icelander.net/downloads and the zazzcat/homebrew-z tap: [Release use cases](../release/.dev/use-case.json), `publish-a-release`.

## How they reach it

Running `release.sh` from the continuous_integration_and_delivery repository on their own Apple Silicon Mac, signed in to GitHub with `gh` and with the release credentials in its environment, which creates the tags and the GitHub releases; merging pull requests into main; and the tap repository zazzcat/homebrew-z.

## Not to be confused with

The [Developer](developer.md), who installs and runs the released tools. The repository does not tell them apart: the owner is a developer too, and the difference is only in who can run the release, create tags, merge and hold the release credentials.
