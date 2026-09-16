# CLAUDE.md

5W: a review-gated task queue in a markdown file, stacked worktrees, and a ship that lands only the
reviewed change. Rust, standard library only, one static binary `5w`. [README.md](README.md) is the
user's view; [PROTOCOL.md](PROTOCOL.md) the queue's rules. This repository manages its own work with
5W: [TASKS.md](TASKS.md) is the queue.

## Working here

- **Work goes through the queue.** `5w ready`, then `5w delegate <id>` for the brief, which carries
  the steps. Every change is a branch in its own worktree (`5w wt new <area>/<what>`); finish with
  `5w submit <id>` and stop — never accept, close or ship work you were handed. `require_task` is on:
  a branch no task names does not ship.
- **Use the binary this repository builds** when the change you need is not released yet
  (`cargo build --release`, then `target/release/5w`). Queue commits must be signed, and only 5w
  0.1.3 or later signs them.
- **Identity and signing:** every commit and tag is `mmmeon <si@mmmeon.com>`, signed with subkey
  `3491A839212CC7DB` (`commit.gpgsign` and `user.signingkey` are set in this repository). History
  was rewritten once to remove another identity; do not reintroduce it.

## Code

- **No dependencies.** The TOML subset, the argument parsing and the glob matching are hand-written on
  purpose; a crate is not the fix for a missing feature.
- **Tests are end to end** (`tests/flow.rs`): scratch git repositories driven through the binary. A
  fix starts as a test that fails without it — prove that by stashing the fix and running the test.
  `FIVEW_TEST_BIN` runs the suite against a release artifact.
- **Nothing project-specific in behaviour.** Lane names, games and forges are configuration or
  examples (`examples/ara.toml`); kinds carry behaviour.
- **Output is read by agents:** compact off a terminal, refusals one line naming the fix, reasoning
  in the README rather than in error text.
- **Keep the two protocols in step:** `templates/PROTOCOL.md` is what `init` and `update-files`
  install; `PROTOCOL.md` here is this repository's stamped copy. `5w doctor` notes a drift.

## Releasing

Bump `Cargo.toml`, then `ci/tag-release.sh` (reproducible build; signed tag whose message is
SHA256SUMS), push `main` and the tag; the release workflow verifies the signature, rebuilds, and
publishes only if its sums match. `ci/sign-release.sh v<version>` attaches `SHA256SUMS.asc`. Details
in the README's *Release binaries*.
