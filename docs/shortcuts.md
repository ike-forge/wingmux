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

### Project selected

| Key | Meaning | Rule satisfied |
|---|---|---|
| `I` | scan `[discovery].scan_root` for new projects (discovery rescan) | shifted = stronger variant of `i` |
| `i` | import a project by absolute path | import action |
| `n` | new feature (worktree + branch + agent shell) | create action |
| `x` | remove project (windows killed, worktrees kept) | menu/destructive action, confirms before acting |

### Feature selected

| Key | Meaning | Rule satisfied |
|---|---|---|
| `n` | new feature (sibling) | create action |
| `a` | add a shell to the feature | add action |
| `d` | delete feature (kills windows, removes worktree; `f` = force when dirty) | vim `d` destructive, confirms; `f` differs by position (confirm mode, not navigation) |

### Shell selected

| Key | Meaning | Rule satisfied |
|---|---|---|
| `Enter` / `l` | attach to the shell | domain convention (attach) |
| `s` | quick-send a line to the agent (no leaving the dashboard) | send action |
| `d` | delete the feature holding this shell | vim `d` destructive, confirms |

## Text input (import / new feature / quick-send)

| Key | Meaning | Rule satisfied |
|---|---|---|
| any printable | append to the input | printable keys belong to text input |
| `Enter` | submit | confirm |
| `Esc` | cancel (back to Normal) | leave innermost first |
| `Backspace` | delete a character | accessibility |
| `ctrl-c` | quit, even here | ctrl layer over input |

## Confirm dialogs (delete feature / remove project / scan prompt)

| Key | Meaning | Rule satisfied |
|---|---|---|
| `y` / `Enter` | confirm | confirm, object named in prompt |
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
| wheel over the tree | scroll one screenful (3 rows) without changing focus | wheel scrolls the panel under the cursor |
| click a statusbar entry | run that action | bar entries are clickable |
| click outside an overlay | dismiss it | click outside dismisses |
| click in the detail/preview panels | nothing | those panels are display-only |

Double-click is detected manually (two left clicks at the same spot within
400 ms): the crossterm version here reports no click count on mouse events.

## Tmux back-key (attach & return)

| Key | Meaning | Rule satisfied |
|---|---|---|
| `Enter` on a shell | inside tmux: `switch-client` to the agent session + `select-window`; standalone: terminal handoff to `tmux attach` | domain convention |
| back-key (default `C-o`, config `tmux.back_key`) | jump back to the dashboard window from any agent window | the one configurable binding (avoids clashing with the user's tmux); bound server-wide in every agent session |
| `C-b d` | leave `tmux attach` (standalone) and land back in the dashboard | tmux's own detach; no back-key exists outside tmux |

## Deliberately unbound — and why

An inert key is a decision, not an omission.

- `/` — no search/filter panel exists yet (the tree is small; statuses are
  classified automatically). When a filter lands it takes `/` per the rules.
- `Tab` / `Shift-Tab` — there is exactly one focusable panel (the tree);
  the detail/preview panes are display-only, so panel cycling would have
  nothing to cycle.
- `y` / `v` — no yank or row-selection feature yet (no multi-select model).
- `:` — no command language; every action already has a key.
- `e`, `c`, `w`, `b`, `t`, `E`, `C`, ... — reserved for future actions
  (edit/rename, toggle, etc.); nothing is bound to a letter that has no
  action, so the one-key-one-action table stays unambiguous.

## Where the words come from

Action names on the statusbar, in the help overlay and in this table are the
same strings (see `HELP_GROUPS` in `src/ui.rs` and `bar_entries` in
`src/ui.rs`). Keyboard-only users see the key in the accent colour; mouse
users click the same words.