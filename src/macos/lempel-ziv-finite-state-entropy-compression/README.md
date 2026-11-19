# lempel-ziv-finite-state-entropy-compression (vendored)

An unchanged copy of the LZFSE library from
<https://github.com/david-brandt-the-icelander/lzfse-compression> (`include/`, `src/` and `LICENSE`).
`free-disk-space` compiles it in through its `build.rs` and uses it to write APFS transparently compressed files.

- **Source commit:** `7b52089` (main after PR #33, which added `lzfse_zazzcat_decmpfs_apply`).
- **License:** these files are under the Mozilla Public License 2.0 (`LICENSE`), not the toolbox's Unlicense.
- **Changing it:** change the library in its own repository, then copy `include/` and `src/` here unchanged and
  update the commit above. Never edit these files in place.
