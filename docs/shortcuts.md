# Shortcuts

Every binding in LazyWingman lives in one key table (`App::on_key` in
`src/app.rs`), follows the global TUI rules (`/opt/data/agents-common-instructions/tui/`,
LazyGit / LazyDocker / vim tradition), and is listed here — current state,
not a wishlist. Update this file in the same commit as any binding change.

The same words are used in the help overlay (`?`), the contextual statusbar
and this table, so learning one teaches the other. Motion keys come in their
vim and their accessibility spellings; both route to one code path.

## Motions — tree

Dispatch order (outermost first): ctrl layer (works over text input and
overlays) → active overlay → text input → focused panel → global bindings.

| Key | Meaning | Rule satisfied |
|---|---|---|
| `j` / `k` (or `↓` / `↑`) | move selection down / up | vim motion; arrows = same code path |
| `h` (or `←`) | collapse; jump to parent | vim motion (`h` = left/up a level) |
| `l` (or `→`) | expand; dock the shell | vim motion; `Enter` alias |
| `Enter` | expand / dock | "open / drill into" action key |
| `g` / `G` | first / last item | vim motion; `Home`/`End` alias |
| `PgDn` / `PgUp` | half page down / up | accessibility: same as `ctrl-d`/`ctrl-u` |
| `ctrl-d` / `ctrl-u` | half page down / up | vim motion |
| `ctrl-f` / `ctrl-b` | full page down / up | vim motion (optional, page keys cover it) |
| `Home` / `End` | first / last item | accessibility: same as `g`/`G` |

Page size = the tree's visible rows (statusbar and panel borders
subtracted), recomputed every frame from the stored viewport.

## Actions — global

| Key | Meaning | Rule satisfied |
|---|---|---|
| `r` | refresh — reconcile state against tmux reality | LazyGit/Docker `r` = refresh |
| `ctrl-t` | live theme picker (preview on move, Enter keeps, Esc restores) | ctrl layer; picker lists built-ins first |
| `?` | open the grouped help overlay (the palette) | palette key; `?` is never rebound |
| `Esc` (preview parked) | back to the live tail | leave innermost first |
| `q` / `Esc` | quit | "do not rebind these" |
| `ctrl-c` | quit from ANY state (even inside a popup / text input) | accessibility: quit always; consumed before mode handlers |

### Theme picker (opened with `ctrl-t`)

| Key | Meaning | Rule satisfied |
|---|---|---|
| `j` / `k` (or `↓` / `↑`) | move the cursor one theme, previewing live | same motions as the tree/help |
| `PgDn` / `PgUp`, `ctrl-d`/`ctrl-u` | move 5 themes | accessibility, same code path |
| `Home` / `End`, `g` / `G` | first / last theme | vim + accessibility spellings |
| `Enter` | keep the previewed theme (persisted to the state file) | keeps; app never rewrites config.toml |
| `Esc` / `q` | restore the theme the picker opened with | cancel |
| mouse | click a row selects + previews; double-click keeps; click outside = Esc | clickable palette rows; click outside dismisses |

Built-in themes (the four Catppuccin flavours first, then dracula/gruvbox/ansi)
list before user themes from the themes dir — the first rows are the same in
every project. The picked theme survives restarts via `state.json`.

### Add-shell picker (opened with `a` on a worktree)

| Key | Meaning | Rule satisfied |
|---|---|---|
| `j` / `k` (or `↓` / `↑`) | move the cursor one entry | same motions as the tree/help |
| `PgDn` / `PgUp`, `ctrl-d`/`ctrl-u` | move 5 entries | accessibility, same code path |
| `Home` / `End`, `g` / `G` | first / last entry | vim + accessibility spellings |
| `Enter` / `l` | start the selected entry | confirm |
| `1`-`9` | start that entry directly | the list numbers itself, so the numbers are keys |
| `Esc` / `q` | close, creating nothing | cancel |
| mouse | click a row selects; double-click starts it; click outside = Esc | clickable rows; click outside dismisses |

The entries are, in order: the repository's own agent command (so the
pre-picker path is still `a` then `Enter`), the other configured `[[agents]]`
presets, and last a **plain shell** — the user's `$SHELL`, started in the
worktree with nothing sent to it. There is deliberately no free-text command
prompt: a command you want to run once is a command you type in that plain
shell, and a command you want to run often belongs in `[[agents]]`.

## Actions — by selection

The statusbar shows only the entries that do something right now; the help
overlay lists all of them grouped by context. Same words in both.

### Repository selected

| Key | Meaning | Rule satisfied |
|---|---|---|
| `n` | new worktree (git worktree + branch cut from the repository's base branch, + agent shell) | create action |

### Worktree selected

| Key | Meaning | Rule satisfied |
|---|---|---|
| `n` | new worktree (sibling; branched from the base branch, not the checkout) | create action |
| `a` | add a shell to the worktree (picker: the repository's agent command, the other configured agents, or a plain `$SHELL`) | add action |
| `d` | delete worktree (kills windows, removes worktree; `f` = force when dirty) | vim `d` destructive, confirms; `f` differs by position (confirm mode, not navigation) |
| `Enter` | on a one-shell worktree: dock that shell (the folded row *is* the shell). With no shell at all it says so and points at `a` — never a silent no-op | no dead keys |

### Shell selected

| Key | Meaning | Rule satisfied |
|---|---|---|
| `Enter` / `l` | dock the shell: its real tmux pane swaps in beside the tree and takes the cursor | domain convention (open the thing the row stands for) |
| `s` | quick-send a prompt to the agent (multiline editor, Alt+Enter sends) | send action |
| `d` | close this shell (kills its window; the worktree and its other shells stay) | vim `d` destructive, confirms; the object is the selected row |

## Text input (new worktree — single line)

The New Worktree prompt names the branch it will cut from under the name
field (`branches from: <base> (base branch)`), and warns when the configured
base branch is absent and creation falls back to the current checkout.

|| Key | Meaning | Rule satisfied |
|---|---|---|
| any printable | append to the input | printable keys belong to text input |
| `Enter` | submit | confirm |
| `Esc` | cancel (back to Normal) | leave innermost first |
| `Backspace` | delete a character | accessibility |
| `ctrl-c` | quit, even here | ctrl layer over input |

## Quick-send textarea (multiline prompt editor)

Quick-send is a full editor, not a line input — it is the main way to talk
to agents, so it behaves like an editor.

|| Key | Meaning | Rule satisfied |
|---|---|---|
| any printable | insert at the cursor | printable keys belong to text input |
| `Enter` | insert a newline | multiline input: Enter writes, it does not submit |
| `Alt+Enter` / `Ctrl+Enter` | send to the agent (and submit) | the send chord — a plain Enter is needed for editing |
| `Tab` | insert a tab | tab is content, not focus (only one panel) |
| `Backspace` / `Delete` | delete before / under the cursor (joining lines at line ends) | editing |
| `←` `→` `↑` `↓` | move the cursor (wrapping at line ends) | arrows = same code path |
| `Home` / `End` | line start / line end | editing motions; the textarea owns them (they never move the tree) |
| `PgUp` / `PgDn`, `ctrl-u` / `ctrl-d` | page the cursor half a page up/down | accessibility, owned by the textarea |
| `ctrl-f` / `ctrl-b` | page the cursor a full page down/up | accessibility, owned by the textarea |
| `Esc` | cancel (back to Normal) | leave innermost first |
| `ctrl-c` | quit, even here | ctrl layer keeps only quit (and ctrl-t) over the textarea |

Sending uses tmux's paste-buffer with bracketed paste, so multi-line prompts
arrive as ONE paste (embedded newlines are content, not Enter presses) and
the trailing Enter submits.

## Confirm dialogs (delete worktree / close shell)

| Key | Meaning | Rule satisfied |
|---|---|---|
| `y` / `d` | confirm (only when the plain answer can succeed) | confirm, object named in prompt; `d` re-pressed = vim `dd` |
| `n` / `Esc` / `q` | abort (default) | confirmation defaults to "no"; the footer spells only `n` — `Esc` closes every overlay |
| `f` | confirm with force (only when offered, e.g. dirty worktree) | destructive variant, offered not assumed |

`Enter` is deliberately **unbound** here. It means "open / drill into"
everywhere else in the app, so it is the key the fingers press by reflex —
a delete must not answer to it. On a dirty worktree `y`/`d` are unbound
too and the footer drops them: `git worktree remove` refuses without
`--force`, so the plain answer exists only to fail.
| `ctrl-c` | quit, even here | ctrl layer over overlays |

## Help overlay (the palette)

| Key | Meaning | Rule satisfied |
|---|---|---|
| `j` / `k` (or `↓` / `↑`) | scroll the help one line | palette takes over the viewport while open |
| `PgDn` / `PgUp`, `ctrl-d`/`ctrl-u` | half page | accessibility, same code path |
| `Home` / `End`, `g` / `G` | first / last row | vim + accessibility spellings |
| `?` / `Esc` / `q` / `Enter` | close | leave innermost first |

## Mouse

| Action | Meaning | Rule satisfied |
|---|---|---|
| click a tree row | select | click to select |
| double-click a row | same as `Enter` | click on row = `Enter` where unambiguous |
| wheel | scroll the preview 3 lines through the selected pane's scrollback; the tree selection never moves | the wheel belongs to the panel that has something to scroll |
| wheel below the split tier | scroll the tree (the preview is not drawn there) | no dead keys |
| click a statusbar entry | run that action | bar entries are clickable |
| click outside an overlay | dismiss it | click outside dismisses |
| click in the detail/preview panels | nothing | those panels are display-only |

Double-click is detected manually (two left clicks at the same spot within
400 ms): the crossterm version here reports no click count on mouse events.

## Startup diagnostics overlay

Shown ONCE at startup when configuration problems were dropped at load
(the configuration rules: drop the smallest unit, never prevent startup,
and report every drop — never a log line nobody reads).

| Key | Meaning | Rule satisfied |
|---|---|---|
| `Esc` / `q` / `Enter` / `?` | dismiss (boot continues; discovery already ran automatically) | close the overlay, then continue boot |
| `j` / `k` (or `↓` / `↑`) | scroll one line | same motions as the help palette |
| `g` / `G`, `Home` / `End` | first / last line | vim + accessibility spellings |
| `PgDn` / `PgUp`, `ctrl-d` / `ctrl-u` | half page | accessibility ctrl layer |
| wheel | scroll 3 lines | wheel scrolls the overlay |
| click outside | dismiss | click outside dismisses |

The tree selection never moves while the overlay is open (the
accessibility layer routes motions to the overlay, same code path as
help/picker).

## Docking — and everything tmux owns once a pane is docked

`Enter` on a shell is one gesture with two halves: a single `swap-pane`
brings the agent's **real** tmux pane into the dashboard's window beside the
tree, and a `select-pane` leaves the cursor in it. Exactly one pane is
docked at a time — docking another shell sends the current one home first —
and quitting sends it home before the slot is taken down.

| Key | Meaning | Rule satisfied |
|---|---|---|
| `Enter` on a shell | dock: `swap-pane` the agent pane into the slot, `select-pane` into it | domain convention (open the thing the row stands for) |
| `Enter` on the shell that is already docked | just move the focus back into it — no second swap | idempotent action key |
| moving the cursor onto a shell | docks it too, **without** taking the cursor with it | the selection is the subject; looking at a row shows the row |

**Dock on look.** The docked pane follows the tree cursor: land on a shell
and its real pane is beside you, with the keyboard still in the tree so the
next `j` is still a `j`. `Enter` is then only needed to *enter* the pane.
Three things it will not do — it does not fire while more keys are still
queued (so held `j` autorepeat costs one swap at the end of the run, not one
per row); it does not fire at all while the docked pane holds the keyboard,
because swapping a pane out from under someone mid-sentence is the worst bug
this design can produce; and it yields to a shell another dashboard has
docked, which is what keeps two idle instances from trading a pane forever.
There is **no settle timer**: the queue itself says when the burst is over,
so the dock lands with no added latency and there is never a window in which
the panel shows the row the cursor has already left. A row with no shell
under it (a repository, an empty worktree) leaves whatever is docked where
it is — browsing is not a reason to tear the layout down. Nothing is docked
until the user first presses a key: startup must not split furniture into
what may well be the user's own window.

Once the pane is on screen it is a normal tmux pane, so **everything past
the dock is tmux's own binding and deliberately unbound here**: moving
between the tree and the agent (`<prefix> + ←` / `→` / `o` / `;` by
default), scrolling and searching the scrollback (copy mode), selecting and
copying, zooming (`<prefix> + z`) and resizing. LazyWingman binds no
navigation key of its own — the keys the user already knows keep working,
and none of the agent's own keys (`1`/`2`/`3`, typing, image paste) is
intercepted on the way in.

The **back-key** (`tmux.back_key`, default `<prefix> + C-o`) still jumps
from an agent *window* to the dashboard window, and is still bound in the
PREFIX table rather than the root table — tmux key tables are server-wide,
so a root-table binding (`bind-key -n`) would hijack the chord in every
session and break vim's `C-o`, readline and other TUIs. Legacy root
bindings from pre-M9 versions are removed on startup (only the exact
command we bound — user bindings of the same key survive). It is on its way
out: with docking, "back to the dashboard" is `<prefix> + ←` between two
panes of one window.

While a pane is docked the dashboard's own strip shows **the tree and
nothing else**. The preview panel is not narrowed, it is gone: the thing it
existed to show is on screen already, in a real pane on the other side of
the divider, and a second one-poll-behind copy of the agent beside the
agent is the duplication docking exists to end. The preview comes back the
moment nothing is docked — it is still the browse mode, and still the only
view below the split tier.

## Deliberately unbound — and why

An inert key is a decision, not an omission.

- `/` — no search/filter panel exists yet (the tree is small; statuses are
  classified automatically). When a filter lands it takes `/` per the rules.
- `Tab` / `Shift-Tab` — no panel cycling: there is exactly one focusable
  panel (the tree); the detail/preview panes are display-only. In the
  quick-send textarea, `Tab` inserts a tab (see above).
- pane navigation, scrolling, search, selection, copy, zoom and resize —
  all deliberately unbound. Once a pane is docked they are tmux's own
  bindings, whatever the user has them set to; re-implementing any of them
  would mean emulating the agent's terminal, which is the exact thing
  docking exists to stop doing.
- `y` / `v` — no yank or row selection yet (no multi-select model).
- `:` — no command language; every action already has a key.
- `e`, `c`, `w`, `b`, `t`, `E`, `C`, ... — reserved for future actions
  (edit/rename, toggle, etc.); nothing is bound to a letter that has no
  action, so the one-key-one-action table stays unambiguous.

## Where the words come from

Action names on the statusbar, in the help overlay and in this table are the
same strings (see `HELP_GROUPS` in `src/ui.rs` and `bar_entries` in
`src/ui.rs`). Keyboard-only users see the key in the accent colour; mouse
users click the same words.

Every hint is written in one shape — `<action> <key>`, e.g. `cancel esc`,
`attach ↵` — and only the key carries the accent colour, so the eye finds it
without reading the sentence (`ui::Hint`, `ui::hint_line`). Keys with a glyph
form take it from `[icons]` (`enter`, `arrows`), so the `ascii` preset spells
them out instead. Every modal ends with such a hint row, one blank row above
it, inside `MODAL_PADDING` (one column left and right, no vertical padding).