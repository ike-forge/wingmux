# Layout and icons

Rounded corners, bare titles (no background fill), tree guides and icons.
Two panels and a bar — the tree says what exists, the preview says what
is happening, the bar says what you can press:

```
╭ Repositories (2) ────────╮╭ Preview ────────────────────────────╮
│󰝰 repo-a                  ││ live tail of the selected shell,    │
│  ├─  login  ⏸ waiting   ││ refreshed every poll                │
│  └─  search             ││                                     │
│     ├─ ● claude ⠹ working││     ╭────────────────────────╮      │
│     └─ ● zsh ⏸ waiting   ││     │ worktree 'search' cre… │      │
│󰉖 repo-b                  ││     ╰────────────────────────╯      │
╰──────────────────────────╯╰─────────────────────────────────────╯
 n worktree · a shell · d delete · ? help
```

A third panel joins them at the foot of the tree's column once a todo pad
(`t` for the selected repository, `T` global) has something in it — as tall
as the note is, and gone again when the note is:

```
╭ Repositories (2) ────────╮╭ Preview ────────────────────────────╮
│󰝰 repo-a                  ││ live tail of the selected shell,    │
│  ├─  login  ⏸ waiting   ││ refreshed every poll                │
│  └─  search             ││                                     │
╰──────────────────────────╯│                                     │
╭ Todo · repo-a ───────────╮│                                     │
│ rebase search once the   ││                                     │
│ schema lands             ││                                     │
╰──────────────────────────╯╰─────────────────────────────────────╯
 n worktree · a shell · t todo · ? help
```

Panel titles are Title Case and name what the panel holds. The tree is
`Repositories (n)`.

**The preview panel** shows the tail of `capture-pane -e` for the selected
shell — escape sequences included, so the agent's own colouring (spinners,
diffs, syntax highlighting) is what you see. `[preview].colors` decides how
much of it survives: `full` (default) takes every colour exactly as the pane
emits it, `foreground` keeps the text colours but drops background colours
so the panel keeps the theme's background, and `none` renders raw text.
Status classification always runs on the stripped text, so colouring never
changes how a pane is classified.

**Scrolling the preview.** The mouse wheel walks back through the selected
shell's history. The first scroll fetches the pane's whole scrollback (a poll
keeps only enough lines to classify a status) and freezes the view there, so
the polls cannot move the text while you read. A parked panel says so three
ways — the offset in its title (`Preview ↑47`), a lit border, and a `live Esc`
entry leading the statusbar — because a panel that has quietly stopped
following its agent otherwise reads as an agent that stopped. `Esc`, or
selecting another row, returns to the live tail. The wheel never moves the
tree selection, so reading back through an agent's output cannot lose your
place; below the split tier, where the preview is not drawn at all, the wheel
falls back to the tree.

Icons come from `[icons]` (default preset `nerdfont`): a **folder per
repository** — `󰝰` open when the row is expanded, `󰉋` closed when it is
folded, `󰉖` outline when the repository has no worktrees at all — `●` shell,
and **four glyphs for a
worktree's state** — `` untouched, `` carrying work the base branch
does not have, `` all its work merged into the base, `󰗨` its
directory gone. "Carrying work" means commits *or* an unclean working
tree: a worktree an agent has edited but not committed is no longer
untouched, and a merged worktree picks the glyph back up as soon as
something is written into it. The four share one padded column, so names
stay aligned whichever one a row draws. The state is also colour-coded:
new is dim, ahead is the `waiting` colour, merged is `worktree_merged`
(green), and a deleted worktree is the `failed` colour — that row is broken, not
finished. Merged and deleted rows are *settled*: the state glyph keeps its
colour, the worktree name is dimmed, and the shell's status is dropped from
the row altogether. Nothing there needs answering any more — the next step
is deletion — so a leftover `waiting` must not compete with the rows that do
want you. No patched font? Set
`preset = "unicode"` for `▣`/`▤`/`▢` and `○`/`◆`/`✓`/`⊘`. Rows carry
meaning in symbols, labels and position as well as colour.

A repository row also shows its main checkout against the upstream, as
LazyGit does: `↑2` commits to push, `↓5` to pull, nothing when in sync or
without an upstream. The counts are in the accent colour, dimmed when the
last fetch failed (offline, timeout, auth) and they may be stale; a dim
`sync_failed` icon marks the failure. A small dim `◇◈◆◈`
after them turns while a fetch runs. See [architecture](architecture.md#upstream-sync)
for when fetches happen.

Uncommitted changes are deliberately *not* in that icon — a branch with
commits reads the same whether or not there is unsaved work on top. They
still decide whether the delete confirm offers `f` (force).

A worktree whose directory has been removed — by hand, or by an agent
told to merge and clean up after itself — keeps its row until you press
`d`, and draws the deleted glyph while it waits: nothing can be measured
in a path that is not there, and the alternative is a row claiming to be a
fresh cut.

"Merged" is measured against the **local** base branch: a PR merged on the
forge shows as ahead until that merge is in your local `main`. A
squash-merged branch keeps commits the base does not have by object id, so
it also reads as ahead. Wingmux records the commit a worktree was cut
from when it creates it — a worktree it merely discovered (or one from a
state file written before this existed) cannot tell "merged" from "new" and
reads as new until it has commits of its own.

> A Nerd Font glyph is drawn single-width by the Mono font variants, which
> is what the default assumes. If your font draws it double-width, add a
> trailing space to the `worktree_*` values — config values are used
> verbatim and the column pads around them.

#### Every `[icons]` key

`preset` picks the base set; any key below overrides it individually.

| Key | `nerdfont` | `unicode` | `none` | Draws |
|---|---|---|---|---|
| `repository` | `󰉋` | `▣` | — | repository with worktrees, folded |
| `repository_open` | `󰝰` | `▤` | — | ...the same repository, expanded |
| `repository_empty` | `󰉖` | `▢` | — | repository with no worktrees |
| `worktree_new` | `` | `○` | — | untouched: no commits, clean tree |
| `worktree_ahead` | `` | `◆` | — | commits not in the base branch, or a dirty tree |
| `worktree_merged` | `` | `✓` | — | commits all merged into the base, clean tree |
| `worktree_deleted` | `󰗨` | `⊘` | — | the worktree directory is gone |
| `status_spinner` | `⣾⣷⣯⣟⡿⢿⣻⣽` | same | — | frames cycled while an agent works |
| `status_waiting` | `󰏤` | `⏸` | — | agent parked at its prompt |
| `status_blocked` | `󰓛` | `⏹` | — | agent asking for permission |
| `status_failed` | `` | `✖` | — | the agent reported a failure |
| `status_ended` | `` | `⏸` | — | the agent exited, a shell is left |
| `status_idle` | — | — | — | agent running, state unrecognised |
| `status_dead` | — | — | — | the pane is gone |
| `shell` | `●` | `●` | — | before the shell number |
| `cursor` | `▌` | `▌` | `▌` | selection, tree and modals |
| `guide_branch` | `├─` | `├─` | `├─` | guide, non-last child |
| `guide_last` | `└─` | `└─` | `└─` | guide, last child |
| `guide_vertical` | `│` | `│` | `│` | guide, continuing parent |
| `separator` | ` | ` | ` | ` | ` | ` | between bar/help entries |
| `warn` | `⚠` | `⚠` | `⚠` | startup diagnostics |
| `ellipsis` | `…` | `…` | `…` | truncation marker |
| `sync_spinner` | `◇◈◆◈` | same | — | frames cycled, followed by `fetching`, `pushing` or `pulling`, while a repository syncs (the startup fetches show the spinner alone) |
| `upstream_ahead` | `↑` | `↑` | `↑` | before the count of commits to push |
| `upstream_behind` | `↓` | `↓` | `↓` | before the count of commits to pull |
| `sync_failed` | `󰖪` | `✗` | — | after the counts while the last fetch failed (offline, timeout, auth); clears on the next success |
| `enter` | `↵` | `↵` | `↵` | the Enter key, in shortcut hints |
| `arrows` | `↑↓` | `↑↓` | `↑↓` | the arrow keys, in shortcut hints |
| `border` | `rounded` | `rounded` | `rounded` | panel and popup frames |

An empty string draws nothing. `guide_branch`/`guide_last`, the three
`repository*` states, the three `worktree_*` states and the `status_*` set
share a rendered column each and are padded to equal width, so names stay
aligned when one of them is empty. `status_spinner` is one string split into
frames, one character per animation step, and every frame joins the status
column — a narrow frame would make the label jitter once per step.
The rest are used verbatim: add your own trailing space if you want a gap
after one (the default `shell = "● "` has one baked in).

Rows carry no counts — the tree is expanded by default, so the children *are*
the count, and the repository icon says whether a folded repository holds
anything. Missing repositories show `(missing)` dimmed. A worktree Wingmux
has no tmux window for is dimmed *whole* — see **Worktrees without a shell**
below.

**Worktrees without a shell**: every worktree `git worktree list` reports
gets a row, whether Wingmux created it or you did. A worktree it has no
tmux window for — one made by hand, or one whose agent shell you closed —
has nothing to dock, send to or capture, so its row is drawn dimmed and
selecting it says so in the preview instead of showing a pane. It is a
listing, not a dead end: `a` starts a shell in that worktree and it joins the
rest, and `d` deletes it like any other.

**The folded row** (`login` above): the nominal case is one worktree, one
agent, so a worktree with a single shell has no child row at all — its own row
*is* that shell. It carries the shell's status, and `Enter` / `m` enter its
docked pane and send it a predefined message directly. `a` gives the worktree
a second shell, and the subtree appears. Only worktrees with several shells (or
none) are expandable.

The statusbar carries the contextual shortcuts and nothing else — no app
name, no session chip, no repository count. Every shortcut in the app —
status bar, modal footer, help palette — is written the same way, `<key>
<action>` ("esc cancel"), with only the key in the accent colour and glyphs
from `[icons]` where a key has one (`↵`, `↑↓`). On a frame too narrow to
hold them all the entries wrap onto a second bar row instead of losing
their tail at the border. Notices ("worktree 'x' created",
"dock failed: …") appear as a toast at the bottom right of the left column, wrapped to as many as
five rows so a long error stays readable, and fade after a few
seconds. Only errors are drawn in the failure colour; an ordinary notice
takes `title_fg`, so "worktree deleted" never reads as a problem; the log and the diagnostics overlay (`D`) keep the record.

The tree takes a percentage of the frame width, capped at 60 columns — it
holds short names, so on a wide screen the extra width goes to the preview
instead of to empty tree padding. Below 64x12 the tree takes a larger share
of the width; below 44x9 the preview hides and the tree fills the frame;
below 26x7 a "terminal too small" message replaces the UI.
