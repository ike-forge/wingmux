# LazyWingman — build state

**M8 COMPLETE** — all M8 milestones done (M8.1–M8.5) plus the M9 feedback
set (M9.1–M9.5); watchdog: this is the completion marker, ping Ilja.
Delivery zip: /opt/data/lazywingman-2026-08-17.zip.

Resume from this file alone. It is updated at the end of every milestone and
whenever a milestone runs long.

## Current milestone

**M23 — worktrees with no shell are dimmed (COMPLETE).** User feedback:
worktrees LazyWingman did not create get listed (good), but when there is no
tmux window for them they are effectively outside it — no attach, no send, no
preview — and the tree said nothing about that. Reframed during design: the
honest predicate is not provenance but `shells.is_empty()`, because a
wingman-made worktree whose last shell was closed is just as inert; the user
agreed and dropped the "not managed by wingman" wording (and, with it, any
special warning in the delete confirm). Landed in `src/ui.rs` only, no state
schema change: `feature_color` mutes such a feature row whole in `theme.dim`
(prefix, icon and name — the rule `row_color` already applies to dead rows,
so the amber dirty-icon tint yields too; the `◇`/`◆` shape still marks the
worktree state), and `draw_preview` gains a `None if
selected_feature_has_no_shell(app)` arm explaining why there is nothing to
show and pointing at `a`. Unchanged on purpose: the delete confirm, and
`bar_entries`, whose non-solo `Sel::Feature` arm already omitted attach/send.
Tests: `a_worktree_without_a_shell_renders_dim` (reads the cell fg through a
new `row_fg` buffer helper, with the highlight parked on a shell row so the
selection colours do not repaint the assertions) and
`a_worktree_without_a_shell_explains_the_empty_preview`. Docs: README (icon
paragraph + a "Worktrees without a shell" section), DECISIONS (why derived
and not a persisted flag). Gate: fmt clean, clippy -D warnings clean, 223
pass apart from the pre-existing
`legacy_root_back_key_is_unbound_but_user_binding_survives`, which fails the
same way on the untouched baseline in this environment.

Before that: **M22 — `i import` and `I scan` are gone (COMPLETE).** User feedback: both
were believed removed already and were not. Scope-based discovery (M9.3)
imports every repo inside the scope at boot, so `i` (import one absolute
path) and `I` (rescan that same root) were duplicating it for two top-level
keys and two of the four statusbar slots on a project row. Removed: the `i`
and `I` key arms, `BarAction::Import` / `BarAction::Scan` and their bar
entries, `InputKind::ImportProject` with its popup arm, `App::import_project`,
`App::run_scan`, the now-unused `App::scan_root` wrapper (`scan_root_from`
stays — it resolves the boot scope), and both help rows. KEPT: boot discovery
(`boot_discovery_from`, `import_cwd_repo`, `discover_and_import`), the
`[discovery]` config block including `scan_root`, and `[[projects]]`.
Consequence to know: a repo created while LazyWingman runs needs a restart to
appear — `r` reconciles tmux state, not the repo list. Docs: README (key
table, discovery section, config sample), docs/shortcuts.md, DECISIONS, the
`config.toml` template comments. Tests: `i_key_scans_and_imports_new_projects`
is replaced by `the_import_and_scan_keys_are_gone` (both keys inert against a
root that HAS a findable repo), plus
`the_project_bar_offers_no_scan_or_import`; the help/bar wording conformance
tests lost their scan/import arms. Gate: fmt clean, clippy -D warnings clean,
221 pass in parallel apart from the pre-existing
`legacy_root_back_key_is_unbound_but_user_binding_survives`.

Before that: **M21 — the test fixtures share one lock (COMPLETE).** Found while gating
M20, pre-existing since M8: `cargo test` in parallel could DEADLOCK.
`tmux::testutil`'s `TMUX_TEST_LOCK` and `util::testutil`'s `XDG_LOCK` were
taken in BOTH orders — `app::tests` nests `with_fresh_xdg(|| ..tmux..)` and
`with_tmux_server(|| with_fresh_xdg(..))` — so two threads could hold one
lock each and wait for the other. It hung forever (no timeout, no failure),
leaving an orphaned `lwtest-<pid>-<n>` tmux server behind; the workaround was
`--test-threads=1`. Now there is ONE lock, `util::testutil::TEST_LOCK`, taken
through `with_test_lock`, which every process-global fixture goes through.
One lock cannot invert, but it has to survive the nesting, so it is reentrant
per thread: a `DEPTH` thread-local makes a nested call a no-op, and a
`DepthReset` drop guard clears it before the mutex guard is released (so an
unwinding test cannot leave a later fixture unlocked). Test:
`the_shared_test_lock_is_reentrant` — it would hang, not fail, on a plain
mutex. Result: `cargo test` (parallel, no flags) runs in ~8.7s, three times
in a row, 220 pass. The "rotating handful of `ui::tests` render tests flake
in parallel" note under the old M15 was the same root cause — those tests
race on the process-global XDG/TMUX env vars — and is gone too.

Before that: **M20 — UI polish: Title Case panels, unfold by default, per-shell
quick-send drafts (COMPLETE).** User feedback: title-case every pane title,
unfold everything by default, and keep a per-shell quick-send buffer so a
half-written prompt survives `Esc`. Landed late: the work was finished on the
`feat` branch off M14, was never merged, and only landed after M19 — which is
why it carries the higher number (its own commit called itself M15, colliding
with the quick-send polish milestone of that number). Merge fixes: the
close-shell confirm inherited the Title Case rename of the deleted
`remove project` confirm and is now `Close Shell`; `Add Shell` (the M18
prompt, which did not exist on the branch) joined the Title Case set and the
`panel_and_popup_titles_are_title_case` guard; the README layout diagram
takes the branch's `Projects (n)` / `Preview` titles without the `(N
features)` counts M19 removed. Gating: fmt clean, clippy -D warnings clean,
219 tests pass apart from the pre-existing
`legacy_root_back_key_is_unbound_but_user_binding_survives` failure, which
already fails on the untouched baseline in this environment.

Before that: **M19 — `dd` confirms, tree rows drop their counts (COMPLETE).** Two pieces
of user feedback: (1) the delete confirm now takes `d` as a yes alongside
`y`/`Enter` — `d` opened the popup, so `dd` finishes the delete the way vim
does; `f` (force) stays its own key so discarding uncommitted work is never
two presses of the same key. (2) The `(N features)` / `[N shells]` chrome is
gone from the tree: it is expanded by default, so the counts only repeated
the rows right below them. The one fact a *folded* project row could not
show otherwise moved into its icon — `▣` holds features, `▢` is empty
(ASCII: `*` / `o`) — mirroring what the feature icon already does for the
worktree state. Docs: README (layout diagram, icon list, folded-row and
deletion sections), DECISIONS (two entries), docs/shortcuts.md confirm-mode
table. Tests: new `project_rows_swap_the_count_for_a_full_or_empty_icon`
(render) and `d_confirms_the_delete_popup` (live tmux); the multi-shell
assertion in `solo_feature_row_carries_the_status_and_no_child_row` flipped
to assert the count's *absence*. Also live-verified in a throwaway tmux:
`▢ repo-a` → create feature → `▣ repo-a`, `d` `d` deletes it and the row
goes hollow again. Gating: fmt clean, clippy -D warnings clean, 204 tests
green apart from the pre-existing
`legacy_root_back_key_is_unbound_but_user_binding_survives` failure, which
already fails on the untouched baseline in this environment.

Before that: **M18 — `a` asks which command to run (COMPLETE).** User feedback: adding a
second shell that runs the same default agent is not the realistic case —
if the default agent is not enough you want a *different* command. `a` now
opens a small prompt for the command; empty falls back to the project
default, which the popup shows as a hint. Gating: fmt clean, clippy -D
warnings clean, tests green apart from the same pre-existing failure.

Before that: **M17 — delete semantics (COMPLETE).** Three pieces of user feedback:
(1) `worktrees.delete_branch` now defaults to **true** — deleting a feature
takes its branch with it unless the user opts out; (2) `d` on a **shell**
row closed the whole feature with all its shells — it now closes only that
shell (kill the window, drop it from the feature; worktree, branch and
sibling shells stay), while `d` on a feature row still deletes the feature;
(3) the `x remove project` action is gone (bar entry, help row, key,
`ConfirmAction`, `App::remove_project`) — it was not useful. Side fix found
on the way: `add_shell` numbered the new shell by `shells.len()`, so after
closing a middle shell the freed index came back and collided with a live
window on reconcile; the index now comes from `Feature::next_shell_index`
(one past the highest in use). Docs: README key table + config sample +
deletion section, DECISIONS (four entries), docs/shortcuts.md. Gating: fmt
clean, clippy -D warnings clean, tests green apart from the pre-existing
`legacy_root_back_key_is_unbound_but_user_binding_survives` failure, which
already fails on the untouched baseline in this environment.

Before that: **M16 — dead entries are muted whole (COMPLETE).** User feedback: "when a
shell/agent is closed, dead, please mute the whole entry in the tree" —
`<myfeature> dead` should read all-muted, not just the status word. Gating:
fmt clean, clippy -D warnings clean, tests green apart from the pre-existing
`legacy_root_back_key_is_unbound_but_user_binding_survives` failure, which
already fails on the untouched baseline in this environment.

Before that: **M15 — quick-send polish (COMPLETE).** User feedback: the
textarea must autowrap instead of overflowing, the "send to agent (14 rows)"
label is useless, and the shortcut row needs a spacer above it. Note:
`cargo test` in parallel flakes on a rotating handful of `ui::tests` render
tests on this machine, baseline included; `cargo test -- --test-threads=1`
is green apart from the tmux one.

Before that: **M14 — tree width cap + dead-shell detection (COMPLETE).**
User feedback: the left panel gets "overly wide" on a wide screen, and a
shell killed with ctrl-d "does not get detected (still shows as waiting)".

Earlier: **M13 — the preview shows the pane's own colours (COMPLETE).**
User question: "is it possible to have colours in the preview, or is it text
only?" Panes are now captured with `capture-pane -e` and the escapes are
parsed into styled text, so the preview shows exactly what the agent
painted. `[preview].colors` picks how much survives: `full` (default),
`foreground` (text colours kept, backgrounds dropped), `none` (raw text).
Gating green (fmt clean, clippy -D warnings clean, 192 tests with
`--test-threads=1`) — see the progress log entry.

Earlier: **M12 — predefined messages (COMPLETE).** User request: `s` writes a
message, `p` should open a modal of configurable canned messages, with
"continue" and "merge to master, drop the worktree" as the defaults.
Gating green (fmt clean, clippy -D warnings clean, 181 tests with
`--test-threads=1`) — see the progress log entry.

Before that: **M11 — folded solo features (COMPLETE).** User feedback: the
nominal case is one worktree, one agent, so a feature with a single shell
should not ask to be unfolded — and the `@5` window ids next to every shell
were plumbing, not information. A one-shell feature now has no child row at
all: its own row carries the shell's status and acts as that shell (attach,
quick-send, predefined messages, preview).

Before that: **M10 — screen decluttering (COMPLETE).** User feedback: the detail panel
repeated what the tree and the user's own config already said, and the
statusbar carried three things nobody can act on plus a notice that never
went away. Layout is now tree | preview; the bar is shortcuts only;
notices are transient toasts. Gating green (fmt clean, clippy -D warnings
clean, 151 tests) — see the progress log below for the one pre-existing
environment failure.

Before that: **M8 + M9 — COMPLETE**, all committed and pushed.

Post-M9 fix (user report): **mouse input was dead** — see the fix entry in
the progress log.

## M8 — plan (user feedback: hide agent panes in separate sessions, project
## discovery, follow agent-common-instructions, work inside AND outside tmux)

Milestones in order; each must be green (fmt + clippy + tests) and committed
AND pushed before the next starts. Update the progress log + Current
milestone line at the end of every milestone. When ALL are done, mark
"M8 COMPLETE" at the top and write the completion marker.

Run policy (continuation worker): each run consumes close to 80–95% of the
5h rolling token window — work through as many milestones as fit; stop only
when ALL are done (marker written) or the budget is near exhaustion
(~4–4.5h active work). A mid-run stop must leave the progress log precise
enough to resume without re-reading code.

- **M8.1 — dedicated agent sessions (hide panes).** tmux.rs: `Tmux` gains
  optional `inside: Option<Inside>` coords; `list_sessions`, `new_session`
  (first window), per-session `list_windows`; pane listing filtered to
  `wingman-session-*` sessions. Naming: session `wingman-session-<slug>`
  (+`-<hash8>` on collision), windows `lw:<feature>/<n>`. state.rs v2:
  `version`, `Project.session_name` (serde-default migration). app.rs:
  create_feature/add_shell create-or-join the project session; delete_feature
  kills windows (session auto-destroys on last); remove_project kills session
  windows; attach = switch-client + select-window (inside) / handoff stub
  (outside, M8.2); startup reconciles sessions + binds back-key in every agent
  session; poll lists only agent-session panes. Update integration tests to
  session model; add session roundtrip tests. Update ui.rs (statusbar
  "inside/standalone", shell detail shows session). README/DECISIONS/AGENTS
  in same commit.
- **M8.2 — runs outside tmux.** main.rs: no hard "must run inside tmux" error;
  detect() → coordinates or standalone; remove LAZYWINGMAN_NO_TMUX. Attach
  outside tmux = terminal handoff: restore terminal → clientless
  select-window to shell window → `tmux attach -t <agent-session>` (blocking)
  → re-init terminal → continue loop (event loop must support the
  suspend/resume dance). Statusbar shows where the dashboard runs. No
  back-key outside tmux (C-b d detaches). Tests: detect standalone, handoff
  path unit-tested at the command level.
- **M8.3 — project discovery.** Startup: cwd (or ancestor) is a git repo →
  auto-import project + `git worktree list` → features for each worktree
  (0 shells). Not a repo → startup Confirm prompt "scan subfolders for git
  repos?"; scan = recursive walk (depth limit, skip dirs: .git, node_modules,
  target, hidden, .lazywingman-worktrees) for `.git` DIRECTORIES (worktrees
  have `.git` files → skipped, covered by their main repo); results cached in
  `$XDG_CACHE_HOME/lazywingman/discovered.json` (root + timestamp + paths).
  New key `I` = "scan for new projects" (stronger variant of `i` import,
  per TUI guidelines) — rescans config `[discovery].scan_root` (default:
  startup cwd), adds new repos found. Missing projects: keep in state,
  `missing: true`, shown "(missing)" dimmed; re-activate when repo again;
  prune() must not drop them. Config: `[discovery] scan_root`, `max_depth`,
  `skip_dirs`. Tests: scan finds nested repos + skips worktrees/deps;
  missing-flag lifecycle; cache roundtrip.
- **M8.4 — agent-common-instructions conformance.** `docs/shortcuts.md` (every
  binding + rule it satisfies); help overlay grouped + palette wording;
  statusbar contextual hints; config failure behaviour per guidelines (drop
  smallest unit + startup diagnostics overlay instead of hard error);
  accessibility keys (PageUp/Down = ctrl-u/d, Home/End = g/G, ctrl-d/u half
  page); NO_COLOR + ASCII fallback check; small-terminal/resize tests; mouse
  basics (click to focus/select, click bar entries); 4 Catppuccin built-in
  themes (Latte/Frappé/Macchiato/Mocha) + `ctrl-t` live theme picker; config
  layering (project-local `.lazywingman.toml` searched upward). Split into
  M8.4a/8.4b if too big for one run.
- **M8.5/M9 — acceptance.** Full verify (build+test), live-tmux acceptance
  loop against a throwaway server (recreate M7's): create feature in a
  project → assert NO window appears in the dashboard session and the agent
  session exists with the shell window; attach/back inside tmux; attach
  handoff outside tmux (command-level); quick-send; status flip; kill/restart
  wingman → rediscovery via sessions; delete feature → session gone; discovery
  flows (git cwd auto-import, scan prompt, `I` rescan, missing project);
  README rewritten for the new model; delivery zip; completion marker
  "M8 COMPLETE" at the top of STATE.md.

## Progress log

### M20 — Title Case panels, unfold by default, per-shell drafts (COMPLETE)

User feedback, verbatim: "Title case for each pane title (Preview / Quick
Send / Projects (Or just Features if in a single git repo))", "Unfold
everything by default", and "Keep a buffor of the quick send (PER SHELL) …
we can have a guardrail … of for instance 1 million chars".

- **Title Case**: every panel and popup title is a proper name now —
  `Preview` (the `(capture-pane)` implementation detail is gone),
  `Quick Send`, `Themes`, `Messages`, `Help`, `Startup Diagnostics (n)`,
  `New Feature`, `Import Project`, `Delete Feature`, `Close Shell`.
  Body text, hints and the statusbar stay lowercase — only titles changed.
  The tree title follows what the tree actually lists: `Features (n)` in
  flatten mode (a single git repo), `Projects (n)` otherwise, and the count
  is now `visible_projects().len()` rather than every project in state
  (out-of-scope projects have no rows, so they must not be counted).
- **Unfold by default**: `UNFOLD_CAP = 30` replaces the old `≤5` rule and
  `feat_expanded` starts `true`. The cap is now a readability backstop, not
  a cost guard, because boot no longer pays for it: `boot_discovery_from`
  loads only the FIRST visible project's worktrees synchronously (that is
  what the first frame draws) and queues the rest in
  `App::pending_worktrees`. The event loop calls
  `drain_pending_worktrees()` once per iteration — one `git worktree list`
  per frame, with a zero input timeout while the queue is non-empty — so a
  scope with dozens of repos paints immediately and fills in. No threads;
  the loop stays single-threaded.
  - `ensure_worktrees` now resizes `feat_expanded` to match `features`.
    It used to push features without it, and a short vector reads as
    folded, so lazily-discovered features arrived collapsed.
  - **Bug found by the change**: `draw_tree` emitted child rows on `fexp`
    alone while `App::visible_nodes` also skips solo features (one shell →
    the feature row IS the shell). They only agreed because features used
    to default to folded; unfolding by default made the renderer and the
    navigation model disagree on the row count, which breaks the cursor
    and mouse hit-testing. `draw_tree` now skips solo children too.
- **Per-shell quick-send drafts**: `App::drafts: HashMap<String, TextBuf>`
  keyed by pane id — the key `captures` and `statuses` already use. `Esc`
  parks the buffer (cursor and scroll included, so `s` resumes exactly
  where it left off); an empty buffer is removed rather than parked.
  `submit_input` now returns whether it succeeded, and the draft is dropped
  only on delivery — a send that failed (dead pane, no server) keeps the
  text instead of swallowing it. Shells with no live pane get no draft;
  they cannot receive a message. In-memory only: drafts do not go into
  `state.json` and do not survive a quit. Guardrail:
  `TEXTBUF_MAX_BYTES = 1_000_000`, enforced in `insert_char`/`newline`,
  which drop silently past the cap (an error toast per keystroke would be
  worse).
- **Crash fix (pre-existing, in the buffer this milestone extends)**:
  `TextBuf::col` is a BYTE offset — the popup slices `line[..col]` to place
  the caret — but `insert_char` advanced it by 1 and `backspace` removed at
  `col - 1`. Typing any non-ASCII character (an em-dash, a typographic
  quote — routine in agent prompts) left `col` inside a multi-byte
  character, and the next edit panicked the whole TUI on a non-char-boundary
  index. `insert_char` now advances by `len_utf8()`, and `backspace`,
  `move_left`, `move_right` and `clamp_col` go through `prev_boundary`/
  `next_boundary` so every motion lands on a char boundary.
- **Test isolation**: the `ui` test module called `app.startup()`, which
  loads AND rewrites the real `$XDG_STATE_HOME/.../state.json`. The
  fixtures therefore inherited the developer's projects (49 of them here,
  well past the unfold cap) and raced each other through the shared file,
  which made several render tests flaky. None of them needed `startup()` —
  they push their own projects — so it is gone from all five call sites
  plus `seeded_app`. `app::tests` already had `with_fresh_xdg` for the
  cases that genuinely need disk.
- Tests: `panel_and_popup_titles_are_title_case`,
  `tree_title_says_features_when_flattened`,
  `features_are_expanded_by_default`,
  `boot_queues_worktree_loading_instead_of_blocking`,
  `lazily_loaded_features_are_expanded`,
  `quick_send_drafts_are_kept_per_shell`, `a_failed_send_keeps_the_draft`,
  `a_delivered_draft_is_cleared`, `the_quick_send_buffer_stops_at_the_cap`,
  `the_quick_send_buffer_handles_non_ascii`; and the existing
  `many_projects_collapse_by_default_few_expand`,
  `expand_loads_worktrees_lazily_and_only_once`,
  `boot_discovery_live_scans_folder_scope` and
  `enter_on_a_folded_feature_attaches_its_shell` retargeted to the new
  behaviour.

### M19 — `dd` confirms, tree rows drop their counts (COMPLETE)

User feedback, verbatim: "When deleting a feature, I should be able to
confirm with yes or `d`. This is then a vim shortcut, more natural for some
users." and "Can we remove the noisy `(x features)` `[x shells]` that are
written along the tree? Since its unfolded by default this is noise. For
projects, we can use a empty/full folder".

- **`d` confirms.** `Mode::Confirm` accepts `Char('d')` on the same arm as
  `Char('y')` / `Enter`. It applies to both confirms `d` can open (delete
  feature, close shell), because both are reached by pressing `d` — the
  reflex is "press it twice", not "press it twice on a feature". `f` was
  deliberately left off that arm: force delete discards uncommitted work,
  so it must not be reachable by repeating the key that opened the popup.
  The hint row reads `[y/d] yes · [f] … · [n] cancel`.
- **Counts are gone.** `draw_tree` no longer emits `(N features)` on the
  project row nor `[N shells]` on a multi-shell feature row. Both said what
  the very next rows already show — the tree is expanded by default. The
  solo-feature row is unchanged: it still carries its shell's status, since
  that shell has no row of its own.
- **Full/empty project icon.** The only thing the count told you that the
  children cannot is whether a *folded* project holds anything, so that
  moved into the icon: `▣` non-empty, `▢` empty (ASCII `*` / `o`). Same
  trick as the feature icon (`◇` clean / `◆` dirty) — one glyph, one fact
  the row could not otherwise state. `(missing)` stays, moved to a trailing
  span now that nothing follows it.
- Tests: `project_rows_swap_the_count_for_a_full_or_empty_icon` renders a
  project with a feature next to an empty one and asserts both the missing
  counts and the two icons; `d_confirms_the_delete_popup` drives `d` `d`
  against a live tmux server and asserts the shell is actually closed;
  `solo_feature_row_carries_the_status_and_no_child_row` now asserts the
  multi-shell row carries *no* count either.
- Live acceptance in a throwaway tmux (`-L lwvis`, XDG under the
  scratchpad, two demo repos): both projects rendered `▢`, creating a
  feature flipped repo-a to `▣`, the confirm showed `[y/d] yes`, and `d`
  `d` deleted the feature — agent session gone, row hollow again.

### M18 — `a` asks which command to run (COMPLETE)

User feedback, verbatim: "Currently the default shell spawns the 'default
command/agent' (good). When using 'a' (add shell), it's spawn a new shell
with the same default command/agent. I think that use case is not realistic.
If the default agent/shell is not sufficient, we might want to spawn a new
shell with another command."

- `a` no longer spawns on the spot. It opens the existing input popup under
  a new `InputKind::AddShell` (title `add shell`, label `command: `), empty,
  with the project's default agent command rendered underneath as a dim
  `default: <cmd>` hint — an empty submit runs that default, so it has to be
  visible. Esc creates nothing.
- `App::add_shell` gained a `command: &str` parameter and owns the fallback
  (`""`/whitespace → `proj.agent_command`), so both entry points — the `a`
  key and the statusbar `AddShell` bar action, which now open the same
  popup — cannot disagree. The tmux side is unchanged: the window is still
  spawned with `agent_shell` and the command sent with `send_keys`. The
  toast names what ran: `shell #1 added (ls -la)`.
- `App::add_shell_default()` exposes the selected project's command to the
  renderer; it resolves from the project index of any selection, so the hint
  works whether a project, feature or shell row is selected.
- Sizing fix found by rendering the popup: the prompt's height was a fixed
  18% of the frame, so the extra hint line pushed `Enter to submit · Esc to
  cancel` out of the box on a 24-row terminal. `input_rect()` now sizes the
  popup to its content (lines + 2 border rows) at 60% width — every input
  prompt keeps its footer on short frames, not just this one.
- Tests: `a_opens_the_add_shell_prompt_instead_of_spawning` (prompt opens
  empty, Esc creates no shell), `add_shell_prompt_runs_the_typed_command`
  and `add_shell_prompt_empty_falls_back_to_the_project_default` (both drive
  the real keys against a real tmux server and capture the new pane to check
  which of two marker commands actually ran), plus
  `add_shell_popup_shows_the_default_command_hint` and the new row in
  `input_popup_cursor_sits_on_input_row` (the hint line sits *below* the
  input row, so the caret must not move), and
  `input_popup_keeps_its_footer_on_short_frames`, which renders both a
  hinted and an unhinted prompt at 24/30/40 rows.

### M16 — dead entries are muted whole (COMPLETE)

User feedback, verbatim: "When a shell/agent is closed, dead, please mute
the whole entry in the tree" — `"<myfeature> dead" <- all muted`.

- M14 made death *detectable*; it was still only the status word that
  changed colour, so a dead row was as visually loud as a working one.
  `draw_tree` now picks every span's colour through `row_color()`, which
  swaps in `theme.status_dead` for a dead row: guide, icon, name, shell
  count and status label all mute together.
- The rule aggregates upward, matching how the tree folds: `feature_dead()`
  = the feature has shells and all of them are dead (so the solo-shell row
  the user named is covered), `project_dead()` = the project has features
  and all of them are dead. An *empty* feature or project is never muted —
  it is unstarted, not finished.
- No new theme key: dead rows reuse `status_dead`, which every built-in
  theme already sets to its muted grey, so themes need no update.
- Test: `a_dead_entry_is_muted_across_the_whole_row` renders a solo dead
  feature next to a half-dead one and asserts, cell by cell inside the tree
  panel's borders, that every glyph of the dead feature row and of the dead
  shell row carries `status_dead`, while the live shell and the feature
  holding it do not.

### M15 — quick-send polish: wrapping, no label, spaced hint (COMPLETE)

User feedback, verbatim: "The quick-send needs some improvements: Autowrap
lines (currently it overflows), remove the 'send to agent (14 rows)'
useless, Add a spacer before the shortcuts at the bottom".

- **Soft wrap**: `wrap_line()` in `app.rs` splits a logical line into byte
  ranges that each render within the wrap width, breaking after a space
  where the line offers one and hard-breaking words longer than the width.
  Tabs count as four columns, matching how the renderer expands them.
  `TextBuf` gained `visual_rows()` / `visual_len()` / `visual_cursor()`
  on top of it, and `ensure_visible(visible, width)` now scrolls in
  *visual* rows — a single long line can fill the whole window on its own,
  which the old logical-line scroll could not express. Cursor motions stay
  logical (up/down step a whole line, as in a plain editor); only display
  and scrolling wrap.
- `textarea_wrap_width(viewport_w)` = popup width − 2 borders − 1 column,
  the reserved column being where the cursor sits at the end of a full
  row. Renderer and key handler both call it, so the width that decides
  the scroll can never drift from the width that draws.
- **Label row gone**: the popup title already says "quick-send", so
  "send to agent: (N rows)" was saying it twice and counting rows nobody
  asked about. Content now starts flush under the top border.
- **Spacer before the hint**: the freed label row became a blank row above
  "enter newline · ctrl-s send · esc cancel". `textarea_visible_lines()` is
  unchanged at height − 4 (2 borders + spacer + hint instead of 2 borders
  + label + hint), so the content height did not move.
- Tests: `textbuf_wraps_long_lines_at_spaces`,
  `textbuf_visual_cursor_follows_the_wrap`,
  `textbuf_scrolls_by_visual_rows`, and
  `textarea_popup_wraps_long_lines_and_spaces_the_hint` — the last one
  asserts the popup's right border survives every row (i.e. nothing
  overflows), the label is gone and the row above the hint is blank.

### M14 — tree width cap + dead-shell detection (COMPLETE)

User feedback, verbatim: "There should be a max size to the left-panel
(300/400px?) On my wide screen, it gets overly wide even though there is
not much inside" and "if I kill a shell (ctrl+d), it does not get detected
by lazywingman (still shows as waiting)".

- **Tree width cap**: `TREE_MAX = 48` columns in `ui.rs`. `tree_width()`
  still takes the tier percentage (32% wide / 42% split) but clamps it, so
  a 200-column terminal gives the tree 48 columns and the rest to the
  preview. `draw_split` now takes the width in *columns*
  (`Constraint::Length`) computed by `tree_width()` instead of re-deriving
  a percentage — the renderer and the mouse hit-testing can no longer
  drift, which the old "must match" comment only asked for politely.
- **Dead-shell detection**: tmux destroys a pane when its shell exits
  (`remain-on-exit` is off by default), so `pane_dead` is almost never
  seen — a ctrl-d'd shell simply *disappears* from `list-panes`. `poll()`
  used to `continue` on a pane it could not find, leaving the last live
  status (`waiting`) on screen forever. `classify()` now takes
  `Option<&PaneInfo>` and reads `None` as `Status::Dead`; `poll()` skips
  the capture for a gone pane and keeps its last capture so the preview
  still shows how the shell ended.
- Tests: `a_pane_tmux_no_longer_lists_is_dead`, and `tree_width` at
  400x60 asserts the cap.

### M13 — colours in the preview (COMPLETE)

User question: "is it possible to have colours in the preview, or is it
text only?" It was text-only by choice — `capture-pane -p` strips the
escapes and the panel rendered one flat `Text`. Now the capture keeps them
and the preview renders them, with a config for how much.

- `tmux.rs`: `capture_pane` gained `-e`, so the capture carries the pane's
  SGR sequences. tmux re-emits the active attributes at the start of every
  line, which is what keeps `preview_tail`'s line-wise slicing safe — the
  tail cannot inherit a colour from a line that scrolled off.
- New `ansi.rs`: `strip` (a small CSI/OSC state machine), `is_blank` (a
  padding line may now carry the pane's colours and no text), and
  `styled_text(capture, mode)` which parses the escapes into a ratatui
  `Text` via `ansi-to-tui` 8.0.1 — it sits on `ratatui-core 0.1`, the same
  core ratatui 0.30 uses, so no version split. Default features off
  (`zero-copy` only) keeps `simdutf8` out of the tree. A capture that fails
  to parse degrades to plain text; terminal output is not a protocol we
  control, so it must never blank the panel.
- `config.rs`: `[preview].colors` = `full` (default) | `foreground` |
  `none`, with `no-background` and `text` accepted as aliases. Lenient like
  every other key: `colors = "rainbow"` drops to the default with a
  diagnostic instead of failing startup. Listed in `--config-dump`.
- `app.rs`: the classification tail moved into `recent_tail()`, which
  strips the escapes first — the status patterns match plain text, so a
  coloured `⠋ Thinking…` must classify like an uncoloured one. This was the
  real trap in the change: without it every agent that colours its spinner
  would have gone unclassified.
- `ui.rs`: `draw_preview` renders `ansi::styled_text(&tail, cfg)`. The
  paragraph's theme style stays the base, so uncoloured output still reads
  as part of the panel.
- Tests: `ansi::` unit tests (strip incl. OSC/truncated sequences, blank
  detection, the three modes, modifiers surviving the background drop),
  `ui::preview_colors_follow_the_configured_mode` (buffer cell fg/bg per
  mode + no escape ever reaching the screen), `app::recent_tail_strips_
  colours_and_padding` (a coloured tail classifies like a plain one),
  `tmux::capture_keeps_pane_colours_for_the_preview` (real tmux server,
  end to end), plus config parse/salvage/dump tests.
- LIVE SMOKE (real release binary, throwaway `tmux -L lwsmoke`, scratch
  repo + scratch XDG dirs, agent command printing red text and a green
  background): with `full` the dashboard pane came back with
  `^[[38;5;1mRED-SMOKE` and `^[[48;5;2mGREEN-BG`; with `foreground` the red
  fg stayed and no `48;` background was emitted; with `none` the line
  rendered in the theme foreground only.
- Gate: fmt clean, clippy -D warnings clean, 192 tests pass with
  `--test-threads=1`. Pre-existing, unrelated:
  `legacy_root_back_key_is_unbound_but_user_binding_survives` fails on the
  untouched baseline here too (verified with `git stash`), and the `ui::` /
  `state::` tests race each other on the shared XDG state in parallel.

### M12 — predefined messages: the `p` modal + `[[messages]]` config (COMPLETE)

User request, verbatim: `s` "allows us to write a message", `p` should
"open a modal with predefined messages. These must be configurable also",
defaulting to "1. continue / 2. merge to master, drop the worktree".

- **Config**: new `[[messages]]` list (`label` + `text`) with
  `MessageConfig`, loaded through `take_message_list` — the same
  smallest-unit salvage as `[[agents]]`/`[[projects]]`: an entry with
  empty `text` is dropped with a diagnostic, an entry without a `label`
  is listed by its text, a stray key inside an entry is reported
  (`MESSAGE_KEYS`), and `messages` joined `TOP_KEYS` so the section
  itself is not "unknown". Absent key = the two built-in defaults;
  defining any entry replaces the list. `DEFAULT_CONFIG` writes the
  defaults out on first run and `--config-dump` lists the labels.
- **Modal**: `Mode::Messages { index }` opened by `p` on a shell (and by
  the new `BarAction::PredefinedSend` statusbar entry). `j`/`k`/arrows
  move, `Home`/`End`/`PgUp`/`PgDn`/ctrl-u/d/f/b move the cursor (the
  overlay owns the viewport, the tree never moves), `Enter` or the
  entry's digit `1`-`9` sends, `Esc`/`q` closes. No shell selected or no
  configured messages → a toast, never an empty box.
- **Sending** reuses `quick_send` (bracketed paste + Enter), so a preset
  is indistinguishable from a typed prompt to the agent.
- **Folded features** (M11) count as their shell here too: `p` is gated on
  `selected_shell()`, not on `Sel::Shell(..)`, so a one-shell feature row
  opens the modal exactly like `s` sends from it. Its statusbar row and the
  *features* help group list `p` alongside `s`
  (`p_opens_from_a_folded_feature_row`). The help overlay was one row from
  its 40-row budget, so the duplicated `g/G` and `Home/End` rows — same
  description twice — became one row to pay for the new `p` row.
- **Mouse**: click a row to select, double-click to send, click outside
  to dismiss, wheel to move the cursor — same contract as the theme
  picker.
- **Render**: `draw_messages`, content-sized like the theme picker, rows
  numbered with the digit that sends them, footer `Enter send · Esc
  cancel`, min size 34x10 (refused with "too small" below that), ASCII
  fallback clean.
- Tests (11 new): config (defaults kept/replaced/label fallback/empty
  text dropped/unknown key/dump/first-run file), app (`p` gated on a
  shell, motion + Esc, empty list toast, Enter sends the selected text
  and the digit sends its entry — both verified against a real tmux pane
  capture, mouse select/dismiss, motion ownership, folded feature row),
  ui (numbered list, tiny-frame refusal, ASCII, statusbar entry, help
  row). 181 pass.
- **Pre-existing failure, untouched:**
  `tmux::tests::legacy_root_back_key_is_unbound_but_user_binding_survives`
  — the same environmental failure recorded under M10 (confirmed again on
  unmodified HEAD via `git stash`). `cargo test -- --test-threads=1`
  remains the reliable way to run the suite here.

### M11 — folded solo features, window ids dropped (COMPLETE)

User feedback on the tree: "the nominal case for a feature is a single
shell (one worktree == one agent) — could we fold the subtree in that case?
This avoids unnecessary unfolding and visual clutter." Plus: drop the `@XX`
next to each shell.

Model: **a feature with exactly one shell is folded for good.** It is not
"collapsed by default" — there is no child row to unfold, ever, so the row
*is* the agent:

```
▾ ▣ repo-a (2 features)
  ├─   ◇ feat-a  waiting      ← folded: no marker, no child, shell status
  └─ ▾ ◆ feat-b [2 shells]
      ├─ ● #0 working
      └─ ● #1 waiting
```

- `app.rs`: one helper, `App::solo_shell(pi, fi) -> Option<usize>`, is the
  whole rule. `visible_nodes()` emits no `Sel::Shell` child for a solo
  feature (whatever its expand flag says), which fixes navigation, mouse
  row hits and `clamp_sel` in one place since all three read that list.
  `expand_or_attach` attaches instead of toggling; `collapse_or_up` treats
  a solo feature as collapsed so `h` walks to the parent rather than
  flipping a flag nothing renders. `selected_shell()` resolves a folded
  feature row to its shell — that is what makes the preview, `s`
  quick-send, `Attach` and the quick-send submit path work on it; the three
  sites that matched `Sel::Shell(..)` literally now go through it.
- Feature actions are untouched: `a` on a folded feature gives it a second
  shell and the subtree appears by itself.
- `ui.rs`: folded feature rows render with a blank the width of the marker
  (names stay aligned) and the shell's status where `[N shells]` was; shell
  rows lost the `sh.window_id` span; new `shell_status()` helper shared by
  both. `bar_entries` gives a folded row `↵ attach · s send · a shell ·
  d delete · ? help`; the help overlay gained `Enter`/`s` rows under
  *features* documenting the fold.
- Tests: `solo_feature_folds_and_a_second_shell_unfolds_it`,
  `enter_on_a_folded_feature_attaches_its_shell` (real tmux server),
  `quick_send_works_from_a_folded_feature_row`,
  `ui::solo_feature_row_carries_the_status_and_no_child_row` (asserts the
  status, the missing child row, and that no tree row contains `@`). The
  `app_with_shell` / `app_with_capture` fixtures now select the feature row
  — with the fold, `Sel::Shell(0, 0, 0)` on a one-shell feature is not a
  reachable state.
- LIVE SMOKE (real binary, throwaway `tmux -L lwsmoke`, scratch repo +
  scratch XDG dirs): a fresh feature rendered `◇ alpha  idle` with the
  preview following it and the bar showing `↵ attach · s send · …`; `a`
  turned the row into `▸ ◇ alpha [2 shells]` and `l` unfolded it to
  `● #0 idle` / `● #1 idle` — no `@` anywhere.
- Gate: fmt clean, clippy -D warnings clean, 161 tests pass with
  `--test-threads=1`. Pre-existing, unrelated:
  `legacy_root_back_key_is_unbound_but_user_binding_survives` fails on the
  untouched baseline here too (verified again with `git stash`), and the
  `ui::`/`state::` tests race each other on the shared XDG state when run
  in parallel.
### M10 — screen decluttering: detail panel out, bar down to shortcuts (COMPLETE)

User feedback, verbatim: the detail panel "honestly does not bring
anything to the table" (its fields are already elsewhere, "process is
kinda noise"), and on the bottom bar "I only want the shortcuts".

- **Detail panel deleted** (`draw_detail`, ~200 lines). The right column
  is the preview alone. Layout tiers renamed with the panel they lost:
  `Wide` (≥64x12, tree 32%) / `Split` (≥44x9, tree 42%) / `TreeOnly`
  (≥26x7) / `Message`; both split tiers now keep the preview, which used
  to hide first. `tree_width` thresholds are unchanged, so mouse
  hit-testing needed no adjustment.
- **Dirty state folded into the tree.** The feature icon carries it: `◇`
  clean, `◆` uncommitted (ASCII `-` / `*`), dirty tinted with the waiting
  colour. The branch was NOT kept — a feature owns its worktree and thus
  its branch, so the name already says it (user's call).
- **Dirty is cached, never rendered live.** `App::poll_dirty` refills a
  `worktree path → bool` map from the poll tick, at most every 3s, and
  immediately (`force`) after a feature is created or deleted. The
  renderer only reads `feature_dirty_cached`. `feature_is_dirty` (a live
  `git status`) stays for the delete-confirm's `--force` offer.
- **Statusbar is shortcuts only** — the `lazywingman` chip, the session
  name, the project count and the message slot are gone; entries moved to
  the left edge, click hit-ranges preserved.
- **Notices became toasts.** `App::message` is a private `Toast`
  (text + error flag + `Instant`), written through `notify` /
  `notify_err` (which also log) and read through `toast()`, which returns
  `None` past the TTL (4s, 6s for errors). Rendered bottom-right, one
  cell inside the panel below it, suppressed while any overlay is open.
  Previously the field was never cleared — the last notice stayed for the
  whole session, which is what the user was looking at.
- **Dead code that the panel had been keeping alive**: `ui::arrow()`,
  `PaneInfo::{width,height}` (and the `pane_width`/`pane_height` fields in
  the `list-panes` format), and `theme.title_bg`, which now paints the
  toast body instead of the old bar chip — no theme-schema change.
- Tests: `statusbar_is_shortcuts_only`, `toast_shows_then_expires`,
  `dirty_feature_gets_a_filled_icon`; `small_terminals_degrade_gracefully`
  rewritten for the new tiers. 151 pass.
- Verified in a real tmux (`tmux -L lwdemo`, 100x20, this repo): dirty
  worktree renders `◆`, clean ones `◇`, the toast appears on `s` with no
  shell selected and is gone five seconds later.
- **Pre-existing failure, untouched:**
  `tmux::tests::legacy_root_back_key_is_unbound_but_user_binding_survives`
  fails on this machine — confirmed identical on the unmodified HEAD
  (`git stash`), so it is environmental, not from this work. Note also
  that `cargo test` in parallel is flaky here (tests share XDG env
  globals); `cargo test -- --test-threads=1` is reliable.

### Fix — mouse input never reached the app (COMPLETE)

Reported by the user: clicking a project/feature neither selects nor opens
it. Two independent defects, both in `app.rs`; the M8.4a mouse tests passed
throughout because they call `App::on_mouse` directly and so exercised
neither of them.

1. **Mouse reporting was never enabled.** `ratatui::init()` turns on raw
   mode and the alternate screen but NOT mouse capture (verified in
   ratatui 0.30.2 `init.rs`), and `EnableMouseCapture` appeared nowhere in
   the tree — so the terminal never emitted a single mouse event. Evidence:
   the running binary in a throwaway tmux server reported
   `#{mouse_any_flag}=0`.
2. **The event loop dropped mouse events.** It read inside a let-chain —
   `if event::poll(..)? && let Event::Key(k) = event::read()?` — so a mouse
   event was *consumed* by `read()`, failed the `Event::Key` pattern, and
   was discarded; the `else if` arm then polled and read a *different*
   event. Mouse handling could only ever fire by accident.

Fix: `term_init()` / `term_restore()` wrap ratatui's init/restore with
`EnableMouseCapture` / `DisableMouseCapture` (used on the attach-handoff
suspend/resume path too), plus a once-installed panic hook that turns mouse
reporting off before ratatui leaves the alternate screen — otherwise the
shell we hand the terminal back to keeps receiving mouse escapes. The loop
now reads once and matches after, in `dispatch_event()`.

- Regression test `event_dispatch_routes_mouse_events` drives
  `dispatch_event` (the loop's own path), not `on_mouse`.
- LIVE SMOKE (real binary, throwaway tmux server, SGR clicks injected with
  `tmux send-keys -H`): `#{mouse_any_flag}=1` / `#{mouse_sgr_flag}=1`;
  a click on tree row 3 moved the selection cursor there; a double-click
  toggled the project's expansion.
- Gate: fmt clean, clippy -D warnings clean, 150 tests pass with
  `--test-threads=1`. Pre-existing, unrelated to this fix:
  `legacy_root_back_key_is_unbound_but_user_binding_survives` fails on the
  untouched baseline too (the developer's own `~/.tmux.conf` root bindings
  leak into the test server), and a few `ui::` tests race each other on the
  shared `state.json` when tests run in parallel.

### M9 — back-key prefix, textarea quick-send, scope discovery, mise, UI pass (COMPLETE)

All five M9 items landed, each committed + pushed to origin/main:

- **M9.1** — back-key moved from the root table (`bind-key -n`, which
  hijacked Ctrl-o server-wide and broke other TUIs) to the **prefix table**
  (`-T prefix`): `C-b C-o` by default, second half still configurable via
  `tmux.back_key`. A surgical cleanup unbinds any legacy root binding that
  exactly matches our switch-client/select-window command (never a user's
  own binding). Tests: prefix-table registration + legacy unbind.
- **M9.2** — quick-send is a **multiline textarea** (`Mode::Textarea` +
  `TextBuf`): Enter=newline, Tab=tab, arrows/Home/End (wrap at line ends),
  PgUp/PgDn/ctrl-u/d/f/b page, **ctrl-s sends** (M9.6; Alt+Enter also,
  where the terminal delivers it), Esc cancels; accessibility layer yields all motion keys while it is open.
  Delivery switched to tmux **paste-buffer with bracketed paste**
  (`paste_keys`) so multi-line prompts arrive as one paste. Tests:
  TextBuf editing, send chords, paste integration.
- **M9.6** — quick-send bug fixes from live use in tmux: **ctrl-s is the
  send key** (tmux defaults to `extended-keys off`, so Ctrl+Enter is not
  encodable and reaches the app as a plain Enter; Alt+Enter is often eaten
  by the outer terminal — the textarea had no working submit key), and the
  **scroll window is the popup's content height** (`TextBuf` methods each
  called `ensure_visible(1)`, so every Enter scrolled the previous line out
  and only one line was ever visible). `TextBuf` no longer scrolls; the key
  handler calls `ensure_visible(textarea_visible_lines(viewport))` once per
  key. Tests: `textarea_ctrl_s_sends`,
  `textarea_enter_keeps_earlier_lines_visible`.
- **M9.7** — preview panel showed **neither the start nor the end** of the
  agent's output: `draw_preview` rendered the capture from its first line,
  but `capture-pane -S -120` returns a window that *starts* 120 lines back
  in the scrollback and *ends* with the blank padding of the pane's empty
  area — so the panel showed rows 1..21 of the scrollback, mid-history.
  New `ui::preview_tail(capture, inner_height)` drops the trailing blank
  padding and keeps the **newest** `inner_height` lines, so the last
  captured line sits on the row just above the bottom border. (The
  original `PREVIEW_BOTTOM_PAD = 3` gap was dropped later — in live use the
  wasted rows cost more than the flush-on-border look did.)
  Tests: `preview_shows_tail_flush_to_bottom_border` (render assertion on the
  row the last line lands in), `preview_tail_handles_short_captures_and_tiny_panels`.
- **M9.3** — discovery is **scope-based, live, lazy**: only projects inside
  the startup dir (or `[discovery].scan_root`) are listed; scopes never
  mix. Boot inside a repo → single project, features FLAT at top level.
  ≤5 projects expand (worktrees loaded at boot for the visible rows),
  >5 collapse (worktrees loaded on expand, once per project; `prune`
  skips unloaded projects). Scan prompt + discovered.json cache REMOVED.
  Tests: scope filtering, flatten (with/without features), lazy worktrees,
  idempotent live scan.
- **M9.4** — **mise.toml** (single task runner per the updated global
  rules): `[tools] rust = "1.97"`, tasks build/fmt/lint/test/gate/install.
  `mise run install` builds release + symlinks `~/.local/bin/lazywingman`
  (verified live from any dir). AGENTS.md Workflow + Links out updated,
  README Development section rewritten.
- **M9.5** — UI pass from the reference screenshots: `BorderType::Rounded`
  everywhere, titles foreground-only (no bg fill), tree draws real guides
  (`├─`/`└─`, `│` continuation; ASCII `|-`/`'-`/`|`), per-type icons
  (`▣` project, `◇` feature, `●` shell). Verified live in tmux: flat
  single-repo tree, 3-repo expanded, 7-repo collapsed.

**Verify:** `cargo fmt --check` clean, `cargo clippy --all-targets -- -D
warnings` clean, `cargo test` **149 green**, `hermes verify` OK (build +
test PASS). Live smoke-tested against real tmux servers (unicode + ASCII
modes). Delivery zip rebuilt; marker "M9 COMPLETE" set.

### M8.5 — acceptance: full verify + live-tmux loop + README + delivery zip + marker (COMPLETE)
- [x] Full verify: `cargo fmt --check` clean, `cargo clippy --all-targets --
      -D warnings` clean, `cargo test` 142 green (no ignored tests).
- [x] LIVE ACCEPTANCE LOOP executed for real against a throwaway tmux server
      (socket `lw-acc8`, session `acc`, XDG under /opt/data/.lw-accept8,
      demo repos /opt/data/lw-demo/{repo-a,repo-b,repo-c}, simulated agent
      `/opt/data/lw-demo/demo-agent.sh` with a `demo` preset in the test
      config):
  1. Dashboard inside tmux → startup scan prompt (cwd not a repo) → `y` →
     repo-a + repo-b imported ✓
  2. Feature created in a project → dashboard session gained ZERO windows
     (only `lazywingman`), agent session `wingman-session-repo-*` exists
     with window `lw:<feature>/0` ✓ (the M8.1 core promise)
  3. Attach inside tmux: real client attached (pty) → `Enter` on the shell →
     client `switch-client` to the agent session (verified via
     `list-clients`) ✓; typed a message into the agent → echoed ✓
  4. Back-key: bound server-wide at session creation (verified in the root
     key table: `bind-key -T root C-o switch-client -t acc \; select-window
     -t @0`) → client byte-injection of `C-o` returned the client to the
     dashboard ✓ (M8.1 noted pty injection was blocked by the host
     guardrail; this run injected into the client's pty via the process
     tool and it worked)
  5. Quick-send `busy 4` → status flipped waiting→working→waiting live ✓
  6. Restart (kill + relaunch): features + shells rediscovered from the
     live `wingman-session-*` sessions ✓
  7. Delete feature: confirm → windows killed → worktree removed → session
     auto-destroyed (only `acc` left) ✓
  8. Discovery flows: `I` rescan found a NEW repo-c ✓; missing project
     (dir moved away) → `(missing)` dimmed + detail "missing — path is not
     a git repo" → restore → re-activated ✓
  9. Standalone boot (fresh pty, no tmux server): auto-started the server
     (`exit-empty off`), scan prompt → 3 repos imported ✓
- [x] README rewritten/updated for the final model (discovery + config
      layering + diagnostics overlay pointer; keybindings table includes
      the startup diagnostics keys). DECISIONS + shortcuts.md updated in
      the M8.4d commit.
- [x] Delivery zip → /opt/data/lazywingman-2026-08-17.zip (Telegram send
      not possible from this session; path reported in the final message).
- [x] Completion marker "M8 COMPLETE" written at the very top of STATE.md
      and committed+pushed (the watchdog pings Ilja on Telegram).

### M8.4d — config conformance: drop-smallest-unit + startup diagnostics overlay + project-local layering (COMPLETE, committed)
- [x] Layering per configuration.md: defaults ← user config
      (`$XDG_CONFIG_HOME/lazywingman/config.toml`, or `--config <path>` /
      `LW_CONFIG`) ← project-local `.lazywingman.toml` (walked upward from
      cwd, stopping at the FIRST hit AND at a git repo root — a sibling
      repo's config is never inherited) ← its gitignored sibling
      `.lazywingman.local.toml` (highest of the file layers). Merge is KEY
      BY KEY: maps merge deeply per subkey, lists/scalars replace outright.
      `Config::sources` records the winning layer per dotted key for
      `--config-dump` ("key = value  # from <layer>").
- [x] User-only keys (configuration rule "not every setting is local-able"):
      `tmux.back_key` (global tmux binding), `worktrees.root`,
      `discovery.scan_root` (machine paths) — rejected in project/local
      files with a diagnostic, never silently applied.
- [x] Failure behaviour: a malformed config NEVER prevents startup. Drop the
      smallest addressable unit — whole-file TOML syntax error drops that
      file's contribution; a bad `[[agents]]`/`[[projects]]` entry drops
      that entry (others kept); a bad scalar or bad section FIELD (one bad
      binding) defaults on its own while the section survives; a bad status
      pattern regex is dropped at load (typed values only reach the app);
      unknown keys are reported, never fatal. Every drop lands in
      `Config.diagnostics` with what/where/used-instead.
- [x] Startup diagnostics overlay (`Mode::Diagnostics`): shown ONCE at
      startup when diagnostics exist; Esc/q/Enter/? dismiss, j/k/arrows +
      g/G + PgUp/PgDn + Home/End + wheel scroll via the SAME accessibility
      code path as help/picker (tree selection never moves); a startup
      confirm (discovery scan prompt) parked behind it resumes after
      dismissal; click outside dismisses; ascii-safe; tiny frames refused
      with the standard message.
- [x] `--config-dump` prints effective values + layer provenance and exits
      before the TUI; `--config <path>` and `LW_CONFIG` override the user
      file (CLI wins); missing explicit override is a diagnostic, not a
      crash. Theme load failure (picked or configured) is lenient too:
      falls back to the built-in default + diagnostic.
- [x] Tests: 142 green (was 116): lenient salvage (whole-file error, bad
      scalar/section-field/agent-entry/list-element, per-field survival,
      unknown keys top+nested, bad regex dropped at load, built-in presets
      kept when list absent); layering (project overrides one key, deep map
      merge, list replace, upward search, stop at repo root, first hit
      wins, local sibling wins, user-only rejection, LW_CONFIG/--config
      override, missing override diagnostic, dump provenance); app-level
      diagnostics overlay (opens/resumes confirm/skipped when empty, keys
      scroll+dismiss, a11y motions route to overlay not tree, wheel, click
      outside dismisses+inside keeps); ui render (all lines + rect + count
      title, scroll window + "j/k scrolls" hint, ascii clean, tiny frame
      refused). clippy -D warnings + fmt clean.
- [x] LIVE SMOKE (real binary + throwaway tmux server, XDG under
      /opt/data/.lw-m84d-live): boot with a deliberately broken user config
      (`theme = 42`, `poll_interval_ms = "fast"`, bad agents entry +
      unknown key) → startup diagnostics overlay lists all 4 dropped units
      with file + cause; Esc dismisses → normal tree; q quits clean;
      `--config-dump` from inside a project dir shows `theme ... # from
      project`, `poll_interval_ms ... # from local`, `base_branch ... #
      from project`; boot with `theme = "no-such-theme"` → overlay shows
      the fallback diagnostic, app boots with the default theme.

### M8.4c — themes: Catppuccin set + ctrl-t live picker (COMPLETE, committed)
- [x] Four Catppuccin built-ins (latte / frappe / macchiato / mocha) with
      the OFFICIAL hex values, unmodified (theming rule), mapped onto the
      app's roles (base → background, text/subtext0 → foreground/dim,
      surface0-2 → selection/borders, flavour accents → status, lavender →
      accent). `catppuccin-mocha` is the new default (the guideline's
      "usual default"); old configs with `theme = "dracula"` keep working.
      `ensure_builtin_themes` now copies all 7 to the themes dir.
- [x] `ctrl-t` opens a live theme picker (ctrl layer → works from any
      state): j/k + arrows step by one theme, g/G + Home/End jump to
      first/last, PgUp/PgDn + ctrl-d/u move 5, wheel moves 3 — the same
      accessibility code path as the tree/help (mode-dependent target, the
      tree selection never moves while the picker is open). Moving the
      cursor PREVIEWS live (the whole screen repaints with the new
      palette). Enter keeps, Esc restores the theme it opened with. Mouse:
      click a row selects + previews, double-click keeps, click outside =
      Esc. Popup sizes to content; tiny frames refuse it with the standard
      message.
- [x] Picker lists built-ins FIRST (stable order: mocha, macchiato,
      frappe, latte, dracula, gruvbox, ansi — same first rows in every
      project), then user themes from the themes dir, sorted + deduped
      (`theme::available_names`). A user theme shadowing a built-in name
      is listed once (user copy wins on load).
- [x] Persistence per the configuration rules: the pick is saved to
      `state.json` (`State.theme`), NEVER rewriting the user's config
      file; startup honours `state.theme` over the config line (main.rs
      logs both). README/DECISIONS/shortcuts.md updated in the same
      commit.
- [x] Tests: 116 green (was 103): official hexes per flavour + mocha
      default; available_names ordering (built-ins first, user themes
      sorted after, shadow dedupe); ctrl-t opens on the current theme;
      cursor previews + clamps (j/k/arrows/End/Home/g/G/PgUp); Enter
      persists to state (fresh XDG) + Esc restores + nothing persisted on
      cancel; page/g motions never move the tree; mouse select/preview/
      double-keep/outside-restore; wheel by 3; picker renders all names +
      registers popup rect; ascii picker clean; tiny frame refused.
      clippy -D warnings + fmt clean.
- [x] LIVE SMOKE (real binary in a throwaway tmux server): boot → `C-t`
      → picker lists all 7 themes, cursor on current; `j` `j` → preview
      moves (frappe selected); Enter → "theme: catppuccin-frappe" +
      state.json `"theme": "catppuccin-frappe"`; restart → log shows
      `theme=catppuccin-frappe (config 'catppuccin-mocha')` — state wins,
      config untouched; quit clean.

### M8.4b — discoverability: grouped help overlay + docs/shortcuts.md (COMPLETE, committed)
- [x] Help overlay `?` becomes the grouped palette (per discoverability
      rules): five context groups — navigate / projects / features /
      shells / global — listing EVERY action (the palette is the index of
      everything, not the contextual bar which hides inert keys). Same
      words as the statusbar entries; the shared strings live in
      `HELP_GROUPS` (src/ui.rs) with the bar entries, so learning one
      teaches the other (test `bar_entries_and_help_share_wording`).
- [x] The palette is scrollable: j/k + arrows step by line, g/G and
      Home/End jump to first/last, PgUp/PgDn + ctrl-u/d scroll half a
      page, the wheel scrolls 3 lines — all while the overlay is open the
      accessibility layer routes to `help_scroll` instead of the tree
      (same code path, mode-dependent target; tree selection never moves
      while browsing help). Popup sizes itself to the content (max 74
      wide) instead of a fixed percentage; long content clips and the
      title advertises "· j/k scrolls". `open_help()` resets the scroll.
- [x] `docs/shortcuts.md` — every binding + the global TUI rule it
      satisfies (motions, actions by selection, text input, confirms,
      palette, mouse, tmux back-key) and the deliberately unbound keys
      with reasons (`/`, `Tab`, `y`/`v`, `:`).
- [x] Tests: 103 green (was 94): help scrolls with j/k/arrows/g/G while
      selection is preserved; half-page + clamping both ends (no
      underflow/overflow); page motions still move the tree in Normal;
      wheel scrolls help-or-tree by mode; grouped render lists every
      action + all 5 headers; bar/help wording cross-check; short frame
      advertises "j/k scrolls" / tall frame fits; ascii help has no `·`
      `→` box glyphs; help_line_count is the single scroll-height source.
      clippy -D warnings + fmt clean. README keybindings table updated
      (a11y keys, mouse, ctrl-c, shortcuts.md pointer).
- [x] LIVE SMOKE (real binary in a throwaway tmux server): boot inside
      tmux → `?` → all 5 group headers render; snapshot shows grouped
      rows; `q` quits cleanly (exit 0).

### M8.4a — input & rendering conformance (COMPLETE, committed)
- [x] Accessibility keys (one shared code path with their vim spellings,
      dispatched above EVERYTHING — over text input and overlays too, per
      the ctrl-layer rule): PageDown/PageUp = ctrl-d/ctrl-u = half page,
      ctrl-f/ctrl-b = full page, Home/End = g/G = first/last. `g`/`G` are
      normal-mode motions only (printable keys belong to text input).
      ctrl-c now quits from ANY state (it used to type a 'c' into
      popups); page size = visible tree rows from the stored viewport.
- [x] Mouse basics: click a tree row selects it, double-click opens
      (Enter); wheel scrolls the tree (the only scrollable panel) without
      changing focus; click outside an overlay dismisses it (popup rect
      tracked per frame); clicks on the detail/preview panels are inert;
      statusbar shortcut entries are clickable (hit ranges recorded by the
      renderer). NOTE: this crossterm version reports no click count on
      MouseEvent, so double-click is manual (same spot within 400 ms).
- [x] Small terminals degrade gracefully, never crash or draw garbage:
      tiers Full ≥64x12 (tree/detail/preview + bar) → NoPreview ≥44x9
      → TreeOnly ≥26x7 → "terminal too small" message below that. An
      overlay that does not fit is REFUSED with a "frame too small for
      this dialog" box (popup_area = None → any click dismisses). Input
      cursor is only set when it lies on the buffer. Resize is per-frame
      (viewport stored on the App; same tier math used by rendering and
      mouse hit-testing so they can't drift).
- [x] NO_COLOR honoured (no-color.org): when present and non-empty, every
      theme role resolves to the terminal default — a monochrome render,
      not a low-colour one; the UI already carries meaning in symbols,
      labels and position. `theme::load_with(name, truecolor, mono)` lets
      tests pass the decision without touching the process-global env.
- [x] ASCII-only fallback for terminals that mangle box-drawing: triggered
      by `LW_ASCII` (non-empty) or a C/POSIX locale (exact match, libc
      precedence LC_ALL > LC_CTYPE > LANG; `C.UTF-8` stays unicode).
      Tree markers become `v`/`>`, separators `-`, `→`→`->`, `⚠`→`!`; box
      borders are rewritten cell-by-cell to `+ - |` after every panel /
      popup render (styles untouched). Pane capture content is never
      rewritten — only LazyWingman's own labels.
- [x] Statusbar becomes a contextual, clickable shortcut bar (was static):
      entries change with the selection (project: I scan · i import · n
      feature · x remove · ? help; feature: n · a · d · ?; shell: ↵ attach
      · s send · d delete · ?) — keys that would do nothing do not appear;
      hjkl/Esc/q take no slot (assumed knowledge per discoverability
      rules). Same words as the help overlay.
- [x] Tests: 94 green (was 78): accessibility motions incl. ctrl layer
      over a popup + ctrl-c from input; mouse select/double-click/wheel/
      bar-click/overlay-dismiss; tier degradation at 120x40/50x10/30x8/
      20x5; overlay refusal on tiny frames; resize in every mode never
      panics and never leaves the cursor off-buffer; ascii render has no
      box glyphs / unicode render keeps them; tree_width tier math;
      ascii_requested env matrix; NO_COLOR resolves every role to Reset.
      clippy -D warnings + fmt clean. README keybindings table updated.
- [x] LIVE SMOKE (real binary): standalone boot renders; ascii mode
      (LW_ASCII=1) renders with + - | borders and v/> markers; NO_COLOR
      boot renders monochrome; all quit cleanly.

### M8.3 — project discovery (COMPLETE, committed)
- [x] Startup (interactive boot, `App::interactive_boot` set by `run()`, off in
      tests): cwd (or ancestor) is a git repo → auto-import the MAIN repo
      (`Git::repo_root` + `git worktree list` first entry, so booting from
      inside a linked worktree resolves to the main checkout) as a project;
      every linked worktree becomes a feature with 0 shells (deduped by
      worktree path; `branch_short` now used by real code). Not a repo →
      startup Confirm prompt "scan <root> for git repos?", seeded with the
      last scan's repo count from the cache.
- [x] New `discovery.rs`: recursive scan for `.git` DIRECTORIES (repos;
      reported, not descended into), `.git` FILES skipped (worktrees /
      submodules — covered by their main repo), hidden dirs always skipped,
      `[discovery].skip_dirs` configurable, `max_depth` bounds (0 = root
      only), symlinks never followed, sorted+deduped output. Results cached
      in `$XDG_CACHE_HOME/lazywingman/discovered.json` (root + timestamp +
      paths; regenerable, corrupt cache → None).
- [x] New key `I` = "scan for new projects" — stronger variant of `i`
      (shifted = stronger, per global TUI keybinding rules): rescans
      `[discovery].scan_root` (default startup cwd; relative roots resolve
      against cwd), adds new repos, re-prunes so missing projects
      re-activate, SAVES state (bug found in live smoke: imports were
      memory-only and lost on quit — `run_scan` now persists; regression
      asserted in `boot_discovery_arms_scan_prompt_when_cwd_is_not_a_repo`).
- [x] Missing projects: `Project.missing` (serde-default); prune() NEVER
      drops projects — a non-repo path flips `missing: true` (features and
      shells kept, unverifiable while gone), a repo-again path flips back
      (re-activation). Tree shows "(missing)" dimmed; detail shows
      "missing — path is not a git repo"; create_feature/add_shell reject
      missing projects with a clear error.
- [x] Config: `[discovery] scan_root` (""), `max_depth` (4),
      `skip_dirs` (default .git/node_modules/target/.lazywingman-worktrees)
      — documented in the first-run config, README and DECISIONS.
- [x] Tests: 78 green (was 59): scan finds nested repos at depth limit +
      skips worktrees/deps/hidden/depth-overflow; custom skip/depth; root
      itself as repo; sorted/deduped; missing root empty; cache roundtrip /
      corrupt / keyed-by-root; `Git::repo_root`+`main_repo` from
      subdirs+worktrees; prune keeps+reactivates missing projects and keeps
      their features; discover_and_import idempotent + cache; boot prompt
      arms + accept runs scan + persisted; boot auto-import incl. from
      inside a worktree; `I` key; missing-project action guards; ui renders
      "(missing)". clippy/fmt clean.
- [x] LIVE SMOKE (real binary + throwaway tmux server, XDG under
      /opt/data/.lw-m83): (1) standalone boot → discovery prompt →
      `y` → both demo repos imported + persisted + cache written (worktree
      .git file excluded) → quit; (2) boot from INSIDE the linked worktree
      → main repo auto-imported, worktree feature `feat-1` with 0 shells;
      (3) repo deleted → kept in state, tree shows "(missing)", prompt
      shows "(last scan of this root found 2 repo(s))", state.json
      `missing: true` after quit; (4) repo recreated → re-activated
      (`missing: false`). Standalone boot with the fresh binary clean.

### M8.2 — runs outside tmux (COMPLETE, committed)
- [x] main.rs already had no "must run inside tmux" error (M8.1); `detect()`
      returns coordinates or standalone; LAZYWINGMAN_NO_TMUX gone.
- [x] NEW BUG FOUND & FIXED by standalone boot smoke: with no tmux server
      running at all, startup failed hard ("no server running") because
      reconcile → `list-sessions`. tmux 3.5a also exits a server that has
      zero sessions (`exit-empty on` by default), so plain `start-server`
      alone doesn't help — probed all variants. Fix: `Tmux::ensure_server()`
      runs `start-server \; set -g exit-empty off` in ONE client invocation
      (two separate clients race the server's exit) only when `list-sessions`
      reports no server — a pre-existing server (user's own) is never
      touched. Called from `App::startup` when `inside.is_none()`.
      `list-panes -a` on a zero-session server errors "no current target" →
      treated as an empty list (fresh boot is a normal state, not an error).
- [x] Attach outside tmux = terminal handoff (M8.1 code, now command-level
      TESTED): `attach()` standalone → clientless `select-window` to the
      shell's window + `pending_attach` → event loop restores the terminal,
      runs `tmux attach -t <session>` (blocking), re-inits ratatui, resumes.
      Inside tmux → `switch-client`, never a handoff.
- [x] Help overlay: attach hint now conditional — inside tmux shows the
      back-key (C-o → dashboard); standalone shows "terminal handed over to
      tmux attach, detach with C-b d". No back-key exists outside tmux.
- [x] Tests: 59 green (was 55) — `ensure_server_starts_a_server_when_none_running`,
      `standalone_attach_preselects_shell_window_and_requests_handoff`,
      `inside_attach_never_requests_handoff`,
      `standalone_startup_boots_without_a_server`. Shared `testutil` module
      in tmux.rs (one lock for ALL tmux tests in the binary — env vars are
      process-global). clippy/fmt clean.
- [x] LIVE SMOKE (real binary): standalone boot in a pty with NO tmux server
      → "dashboard standalone" + "started tmux server (exit-empty off)" →
      TUI renders → `q` exits 0. In-tmux boot unchanged ("dashboard inside
      tmux: session='dash'") and still renders/quit cleanly.

### M8.1 — dedicated agent sessions (COMPLETE, committed)
- [x] tmux.rs rewritten: `Tmux { inside: Option<Inside> }` (dashboard coords
      optional — standalone supported), session primitives (`list_sessions`,
      `new_session` with named first window, per-session `list_windows`,
      `session_exists`, `kill_session`), pane listing filtered to
      `wingman-session-*` sessions (dashboard panes excluded), clientless
      `select-window -t session:@win`, `switch-client` attach,
      `attach_handoff` (`tmux attach` for standalone), server-wide
      `bind_back_key` (verified: tmux key tables are server-wide; `bind-key`
      has no target flag; two-command binding needs literal `\;` argument).
      Naming: `wingman-session-<slug>` + stable `-<hash8>` of the path on
      collision (`DefaultHasher::new()` is deterministic); windows
      `lw:<feature>/<n>`.
- [x] state.rs v2: `State.version`, `Project.session_name` (serde-default
      migration; v1 files load, v1 shells prune on first v2 reconcile).
      Reconcile walks `wingman-session-*` sessions → windows; project matched
      by session name / slug / hash suffix / pane-cwd reconstruction.
- [x] app.rs: create_feature/add_shell create-or-join the project's agent
      session (`ensure_agent_session`), set `session_name`; delete_feature
      kills windows (last-window kill auto-destroys the session) + kills a
      shell-less session; remove_project kills the session; attach =
      switch-client+select-window inside tmux, handoff flag standalone;
      startup binds back-key when inside; event loop performs the
      restore→attach→re-init handoff dance.
- [x] main.rs: no hard "must run inside tmux" error; detect → coordinates or
      standalone; LAZYWINGMAN_NO_TMUX removed.
- [x] ui.rs: statusbar "dashboard: <session> / standalone".
- [x] Tests: 55 green (was 51) incl. real-tmux session lifecycle (create,
      join, clientless select-window changes active window, last-window kill
      destroys session, dashboard panes excluded from list_panes), detect
      standalone, back-key registration, v1→v2 migration. clippy/fmt clean.
- [x] LIVE SMOKE (real tmux + binary): config-imported repo-a; 'n' →
      "feat-smoke" → dashboard session gained ZERO windows; agent session
      `wingman-session-repo-a` created with `lw:feat-smoke/0`; TUI tree
      renders it; quit + restart → shell rediscovered from its session.
- [x] README sections updated (session model, run inside/outside tmux).

### M7.1 — Feedback fix: input popup cursor offset (committed)
- [x] Bug: the block cursor in the input popups (new feature, quick-send,
      import) was drawn one row BELOW the typed text — `cursor_y =
      area.y + 3` while the input line (`lines[1]`, after the border +
      one spacer line) renders at `area.y + 2`. The caret visually
      belonged to the empty spacer row under the text.
- [x] Fix: `inner` rect's `y` corrected to `area.y + 1` (content starts
      below the border) and `cursor_y = inner.y + 1` (the input row).
      One shared code path (`draw_input`) served all three popup kinds,
      so a single fix covers new feature / quick-send / import.
- [x] Regression test `ui::tests::input_popup_cursor_sits_on_input_row`:
      renders all three input kinds headless and asserts the exact cursor
      position. Verified it FAILS on the old offset and passes on the fix.
- [x] Gating green: fmt clean, clippy --all-targets -D warnings,
      51 tests (50 + 1 new).

### M5+M6 — TUI + actions (committed)
- [x] ratatui 0.30 (rewritten crate: ratatui-core/widgets/crossterm split; Color::Rgb is tuple, Indexed not AnsiValue; Layout::vertical/horizontal).
- [x] Layout: left tree (projects/features/shells), right detail + live capture-pane preview, status bar.
- [x] Modes: Normal / Input (import, new feature, quick-send) / Confirm (delete y/n/f) / Help overlay.
- [x] Keys: j/k/h/l/Enter/i/n/a/s/d/x/r/?/q.
- [x] Actions: import (state+config merge), create feature (worktree add + window + agent cmd), add shell, attach (select-window), quick-send (send-keys), delete (kill windows + worktree remove + optional branch delete), remove project (kill windows, keep worktrees).
- [x] Status polling: list-panes -a + capture-pane per shell → classify (dead>ended>failed>waiting>working>idle).
- [x] Persistence: state.json, reconcile+prune on startup/refresh, window rediscovery + feature reconstruction from pane cwd.
- [x] Back-key binding C-o → dashboard window (configurable).
- [x] Smoke test: TUI renders inside tmux, in-tmux detection works.

### M7 — Harden & verify (complete)
- [x] Acceptance loop EXECUTED for real against a live tmux server
      (session 'acc', XDG dirs /opt/data/.lw-accept, demo repos
      /opt/data/lw-demo/repo-{a,b}, simulated agent demo-agent.sh):
  1. Started in a tmux pane ✓
  2. Imported 2 repos: repo-a via 'i' key, repo-b via config merge ✓
  3. Feature on each: worktree on disk + shell spawned with default cmd ✓
  4. Second shell added to a feature (lw:.../1) ✓
  5. Attach: Enter → select-window to agent window ✓; typed a message into
     the agent ✓; back: binding registered (bind-key -n C-o), select-window
     mechanism proven — interactive keypress simulation was blocked by the
     host security guardrail (couldn't inject bytes into the client pty),
     noted as the only unverified micro-step.
  6. Quick-send 'busy 2' from the dashboard ✓ (agent executed it)
  7. Status flipped waiting→working→waiting live as agent went busy/idle ✓
  8. Restart: state + windows rediscovered ✓ (incl. clean quit, LW_EXIT=0)
  9. Delete feature: confirm popup → windows killed, worktree removed,
     tree updated ✓
  10. Theme switch via config + restart: ansi theme uses only palette
      indices 0-15 (terminal palette); dracula used 38;5;NNN up to 231 ✓
- [x] Bugs found & fixed during the loop:
  * centered_rect destructured 3 rects into [v] → panicked on first popup
    (added headless render test for all modes)
  * capture-pane pads blank lines → recent-tail was empty → 'ended' status
    (trim trailing blanks before classifying)
  * banner text in the 10-line window → stale 'working' (last-line-prompt
    priority in classify)
  * shell-foreground agents (bash running a script) were 'ended' before
    their tail was read (tail checks moved before the shell heuristic)
- [x] clippy --all-targets -- -D warnings CLEAN, cargo fmt applied,
      50 tests green (incl. real-tmux integration tests).
- [x] README.md written (install, keybindings, config, themes, tmux model).
- [x] Delivery zip → /opt/data/lazywingman-<date>.zip (Telegram send not
      possible from this TUI session; path reported in final message).

### M4 — tmux layer (committed)
- [x] Tmux::detect() (in-tmux detection via TMUX/TMUX_PANE + display-message -t pane).
- [x] Window spawn (new-window -d -P -F), send-keys -l, capture-pane, select-window, kill-window, bind-key -n back-key.
- [x] list-panes -a (NOT -t session — that only lists the active window!) filtered by session_id.
- [x] Deterministic window names lw:<proj>/<feat>/<n>, sanitised.
- [x] Real-tmux integration tests (5), with the pitfalls documented:
  * Fake TMUX env breaks window creation (nested-client behavior) — keep unset in tests.
  * `new-window` right after `new-session -d` races tmux server startup
    ("index 0 in use") — new_window now retries 3x; test harness sleeps 150ms.
  * static Mutex inside a GENERIC fn is per-monomorphization → put at module level.

### M2 — Config + themes (committed)
- [x] XDG config loading, commented defaults written on first run.
- [x] Three built-in themes (dracula, gruvbox, ansi) embedded + copied to themes/.
- [x] Truecolor detection + xterm-256 quantisation fallback; ansi theme uses only ANSI-16.
- [x] Agent presets (claude/codex) with per-agent status regexes; per-project overrides; generic fallback.
- [x] 21 unit tests green (config parse, theme load, colour parse/quantise, path normalisation).
- [x] First-run smoke test: config.toml + themes + log file all written, exit clean.

### M1 — Scaffold (done)
- [x] Rust toolchain installed (`~/.cargo`, rustc 1.97.1) — container had none.
- [x] tmux 3.5a installed as non-root: `apt-get download` + `dpkg -x` into
      `~/.local/tmux-prefix`; run with
      `LD_LIBRARY_PATH=$HOME/.local/tmux-prefix/usr/lib/x86_64-linux-gnu`.
      `tmux -L lwtest` verified working headless. (libevent-core/extra/pthreads,
      libjemalloc2 pulled the same way.)
- [x] Repo at `/opt/data/lazywingman` (git, branch `main`, local user config
      `LazyWingman Builder <builder@lazywingman.local>`).
- [x] Deps: ratatui 0.30.2, crossterm 0.29.0, serde/serde_json, toml 1.1.4,
      thiserror 2.0.20, regex 1.13.1, log 0.4.33.
- [x] Module skeleton: `error.rs` (thiserror enum + context), `util.rs`
      (XDG paths), `logging.rs` (file logger, never stdout), `app.rs` stub,
      `main.rs`.
- [ ] `cargo build` green, M1 commit.

## Gotchas / environment notes

- **PATH**: `~/.cargo/bin` and `~/.local/bin` must be on PATH; tmux needs
  `LD_LIBRARY_PATH=$HOME/.local/tmux-prefix/usr/lib/x86_64-linux-gnu`.
- **`hermes verify` recipe** (`.hermes/environment.json`) has `start: null`
  on purpose: this is a TUI with no HTTP endpoint and it refuses to boot
  outside tmux, so the start/readiness phase is skipped. Do NOT re-run
  `hermes verify --save` — detection regenerates `start: cargo run` and
  the readiness poll then fails.
- **TERM**: container TERM=dumb; use `TERM=xterm-256color` for tmux commands.
- **No root**: everything installed into `$HOME/.local` via deb extraction.
- zip: use `python3 -m zipfile -c out.zip dir` (no zip binary).
- User communicates via this TUI session; Telegram send is NOT available from
  a TUI session (no send_message tool). Backup zip will be dropped in
  `/opt/data/` and its path reported in the final message instead.
- Do not print anything to stdout while the TUI is running — it corrupts the
  screen. All diagnostics go to the log file.
