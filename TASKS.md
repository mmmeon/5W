# Tasks

The queue. One task per line, read and written by `5w`:

```
- [ ] #<id> <what to do>  @<area> !<1-4> ><lane>  needs:#<id>  branch:<area>/<what>
  An optional body: every line indented two spaces under a task belongs to it.
```

`[ ]` open · `[~]` submitted, waiting on review · `[x]` accepted or closed, with `via:` saying how.

Ids are permanent — never renumber, never reuse. Edit this file through `5w`; without it, follow
[PROTOCOL.md](PROTOCOL.md), which `5w lint` and the pre-commit hook check.

## Open

- [ ] #1 Attach SHA256SUMS.asc to v0.1.2: dist/SHA256SUMS.asc is signed, the gh token lacks Contents write @release !1 >restricted
- [ ] #2 Create the report label on mmmeon/5W so 5w report send can apply it @report !1 >restricted
- [ ] #3 Release 0.1.3: queue commits and squashes signed when the repository signs @release !1 >restricted
- [ ] #4 CI for 5W itself: cargo fmt, clippy and test on every push and change request, and the 5w ci check @ci !2
- [ ] #5 Split titles cut at a word boundary end in a misleading …: the rest is in the body, not lost @queue !1
- [ ] #6 Normalize worktree paths: wt new prints /home/who/r/ara/../ara-wt/… @wt !1
- [ ] #7 Test the panic hook: a crash records last-failure.md and says how to report it @report !2
- [ ] #8 Run the aarch64 release binary on real arm64 hardware @release !1 >manual

## Done
