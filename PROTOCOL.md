<!-- 5w 0.1.3 protocol -->
# Task protocol

How the queue in `TASKS.md` is edited. The `5w` tool follows these rules for you; without it, you
follow them by hand, and `5w lint` (run by the pre-commit hook where installed) checks that you did.
If `5w` is installed, use it instead of editing.

## Rows

```
- [ ] #14 Short title, at most 120 characters  @area !3 >agent needs:#12,#13 branch:area/what
  Optional body. Every line indented two spaces under a row belongs to it.
```

- Marks: `[ ]` open · `[~]` submitted, waiting on review · `[x]` closed.
- Fields follow the title, space-separated, any order: `@area`, `!1`–`!4` complexity, `>lane`,
  `needs:#a,#b`, `branch:<name>`, `rework:"<reason>"`, `via:<how>`, `submitted:<sha>`,
  `reviewed:<sha>`. A field value holds no spaces; `rework` is double-quoted with `\"` for a quote.
- Lanes are named in `.5w.toml`, each with a kind: `agent` (anything can do it), `restricted`
  (needs a machine, an account or a secret), `manual` (a person, by hand, outside the repository),
  `decision` (the owner's call). A `decision` lane closes `via:decided`, every other `via:self`.
  No `>lane` means the default lane, `>agent` unless configured.
- Open rows sit under `## Open` (or their lane's own heading); closed rows under `## Done` or in the
  archive file `DONE.md`, never in both. Lines inside ``` fences are never rows.
- A `<sha>` is the full commit name, `git rev-parse <branch>`. A shorter prefix (7 or more hex
  digits) that an older row already holds still reads, while exactly one commit starts with it.

## Edits, and what each must carry

| Change | Mark | The row must |
|---|---|---|
| Add | new `[ ]` | take an id greater than every id in `TASKS.md` and `DONE.md`, fenced ones included; carry no `via:`, `submitted:`, `reviewed:` or `rework:` |
| Submit (whoever did the work) | `[ ]`→`[~]` | gain `branch:` and `submitted:` = that branch's tip. The branch has no uncommitted work |
| Accept (the reviewer) | `[~]`→`[x]` | gain `via:review` and `reviewed:` = the commit you reviewed; lose `rework:`; move to `## Done` |
| Reject (the reviewer) | `[~]`→`[ ]` | gain `rework:"why"`; lose `submitted:` |
| Close without review | `[ ]`→`[x]` | gain `via:` = its lane's close word; move to `## Done`. Never for work someone else did |
| Reopen | `[x]`→`[ ]` | lose `via:`, `reviewed:`, `submitted:`; move back to its open heading |
| Retitle, re-field | `[ ]`→`[ ]` | change anything but the id and the closure fields; gain no `rework:` — only a reject adds one |
| Archive | `[x]` row | move, byte for byte, from `TASKS.md` to `DONE.md` |
| Unarchive (to reopen it) | `[x]` row | move, byte for byte, from `DONE.md` back under `TASKS.md`'s `## Done`, in a commit `chore(tasks): unarchive #<id>` that changes no row |

Never:

- change or delete a closed row, other than reopening, archiving or unarchiving it;
- delete any row, or reuse or renumber an id;
- close, submit or accept a `decision` row unless you are the owner, or a `manual` row nobody did
  by hand;
- accept or ship your own delegated work — submit it and stop.

## Commits

- A queue edit is **its own commit**, on the **trunk**, touching only `TASKS.md` and `DONE.md`.
  A merge carries each side's rows as they are. Resolving a conflict, a row both sides changed keeps
  every field either side changed (a close wins over edits made while open), and a row both sides
  added under one id is renumbered past both sides' highest id; any other change in a merge is a queue
  edit inside it.
  Branches carry no queue changes. Both are plain files, never symlinks — on the trunk or in its checkout.
- Message: `chore(tasks): <verb> #<id>` — `add`, `submit`, `accept`, `reject`, `close`, `reopen`,
  `unarchive`;
  the commit changes no other row.
- Several edits may share one commit (`5w batch`), each row edited at most once in it. Its subject
  names every row it changes and no other, `chore(tasks): <verb> #<id>, <verb> #<id>`; its body
  holds each edit's message.
- A branch lands only after its row is `[x] via:review`, by fast-forward, and only if what it adds
  is still exactly what was reviewed at `reviewed:`. Anything added after review needs a fresh
  submit and accept.
- A CI job may make the submit and accept edits for a change request (`5w ci --event`): the same
  edits, pushed to the trunk, from a job that runs nothing from the change request. Accept is then
  the forge's approval of the change request's tip, and the forge decides whose approval counts.
