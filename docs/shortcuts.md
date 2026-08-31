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
| `l` (or `→`) | expand; attach to shell | vim motion; `Enter` alias |
| `Enter` | expand / attach | "open / drill into" action key |
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
| `a` | add a shell to the worktree (prompts for the command; empty = the repository default) | add action |
| `d` | delete worktree (kills windows, removes worktree; `f` = force when dirty) | vim `d` destructive, confirms; `f` differs by position (confirm mode, not navigation) |

### Shell selected

| Key | Meaning | Rule satisfied |
|---|---|---|
| `Enter` / `l` | attach to the shell | domain convention (attach) |
| `s` | quick-send a prompt to the agent (multiline editor, Alt+Enter sends) | send action |
| `d` | close this shell (kills its window; the worktree and its other shells stay) | vim `d` destructive, confirms; the object is the selected row |

## Text input (new worktree / add shell — single line)

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
| `y` / `d` / `Enter` | confirm | confirm, object named in prompt; `d` re-pressed = vim `dd` |
| `n` / `Esc` / `q` | abort (default) | confirmation defaults to "no" |
| `f` | confirm with force (only when offered, e.g. dirty worktree) | destructive variant, offered not assumed |
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

## Tmux back-key (attach & return)

| Key | Meaning | Rule satisfied |
|---|---|---|
| `Enter` on a shell | inside tmux: `switch-client` to the agent session + `select-window`; standalone: terminal handoff to `tmux attach` | domain convention |
| `<prefix> + back-key` (default `C-b C-o`, config `tmux.back_key`) | jump back to the dashboard window from any agent window | the one configurable binding (avoids clashing with the user's tmux); bound in the PREFIX table — never the root table, so no other TUI ever loses the key |
| `C-b d` | leave `tmux attach` (standalone) and land back in the dashboard | tmux's own detach; always works outside tmux |
| `<prefix> + back-key` during a standalone attach | detach and land back in *this* dashboard | key tables are server-wide, so the chord is borrowed for the handoff (and restored after) rather than pointing at another lazywingman's window; a binding that is not ours is left alone |

Why the prefix: tmux key tables are server-wide, so a root-table binding
(`bind-key -n`) hijacks the chord in every session — vim's `C-o`, readline,
and other TUIs running inside tmux would all lose the key. Behind the
prefix, the chord only fires when the user explicitly asks for a tmux
command. Legacy root bindings from pre-M9 versions are removed on startup
(only the exact command we bound — user bindings of the same key survive).

## Deliberately unbound — and why

An inert key is a decision, not an omission.

- `/` — no search/filter panel exists yet (the tree is small; statuses are
  classified automatically). When a filter lands it takes `/` per the rules.
- `Tab` / `Shift-Tab` — no panel cycling: there is exactly one focusable
  panel (the tree); the detail/preview panes are display-only. In the
  quick-send textarea, `Tab` inserts a tab (see above).
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