# Graph Report - lithe  (2026-10-11)

## Corpus Check
- 45 files · ~92,215 words
- Verdict: corpus is large enough that graph structure adds value.
- Unclassified: 3 file(s) not represented in the graph (top: (none) 1, .wgsl 1, .toml 1)

## Summary
- 1689 nodes · 3718 edges · 88 communities (68 shown, 20 thin omitted)
- Extraction: 98% EXTRACTED · 2% INFERRED · 0% AMBIGUOUS · INFERRED: 86 edges (avg confidence: 0.89)
- Token cost: 102,159 input · 25,735 output

## Community Hubs (Navigation)
- Tab and Diff Workspace Model
- App Event Loop Tests
- Language Registry and Parsing
- Layout and Hit Testing
- Command Palette Vocabulary
- App Commands and LSP Requests
- Markdown Reader Tests
- Source Control Panel
- File Tree and Inline Naming
- Unified Diff Construction
- Fuzzy Picker
- Editor Edit Tests
- Settings Persistence
- Context Menu Actions
- Cursors and Selection
- Git Write Operations
- Recent Projects
- Buffer and Project Search
- Git Repository Reads
- Pane Drawing and Refresh
- Markdown Inline Scanner
- Editor Core
- wgpu Renderer Setup
- Status Bar Icons
- Hit Regions
- Glyph Atlas
- LSP Protocol Framing
- Drawing Entry Points
- Rope Text Buffer
- Pending LSP Replies
- Live Language Server Tests
- Context Menu Model
- Text Buffer Tests
- Find Bar and Prompt
- Server Process Lifecycle
- Markdown Block Layout
- Landing Page
- LSP Client Requests
- Image Decoding and Fitting
- Theme Colours and Diff Drawing
- Settings Page Rows
- Language Server Manager
- Undo History
- Git Blame
- Performance Invariants
- Diff Geometry and Scene
- App Icon Generator
- GPU Frame Batching
- Whole-Line Commands
- Indentation and Line Opening
- Cursor Motions
- Colour and Frame Drawing
- Font Faces and Glyph Sizes
- LSP Request Kinds
- Native Menu Bar
- Explorer Context Menu
- Find Query State
- File Operations Undo
- App Startup and Wake
- Timers and Idle CPU
- LSP Document Sync
- Preview Decisions
- Comment Toggling
- Project Search Worker
- Preview and Hover Drawing
- Crate Layout
- Refactoring and Tree Editing
- Completion Popup
- Saving and Window Lifecycle
- Atlas Constants
- Go-To Request Kinds
- Request Outcome States
- Diff Line Kinds
- Markdown Block Decoration
- Markdown Tables
- Project Scope and Milestones
- Syntax Highlighting Limits
- Diagnostic Severity
- LSP Advanced Requests
- Discard Confirmation
- Markdown Render Tests
- Menu Routing
- Test Temp Directories
- Rope Tree-sitter Adapter
- Character Classes
- Mac Bundle Script

## God Nodes (most connected - your core abstractions)
1. `App` - 80 edges
2. `Editor` - 70 edges
3. `Pane` - 65 edges
4. `render()` - 55 edges
5. `Command` - 53 edges
6. `TextBuffer` - 52 edges
7. `Layout` - 39 edges
8. `Action` - 38 edges
9. `Workspace` - 38 edges
10. `ed()` - 37 edges

## Surprising Connections (you probably didn't know these)
- `A throwaway Syntax per fence` --rationale_for--> `highlight_fence()`  [INFERRED]
  docs/ARCHITECTURE.md → crates/app/src/markdown.rs
- `Context menus built per click` --rationale_for--> `Action`  [INFERRED]
  README.md → crates/app/src/context.rs
- `--snapshot frame capture` --rationale_for--> `snapshot()`  [INFERRED]
  README.md → crates/app/src/main.rs
- `Markdown preview reader` --rationale_for--> `highlight_fence()`  [INFERRED]
  README.md → crates/app/src/markdown.rs
- `A run boundary is part of a word, not a break` --rationale_for--> `word_boundary()`  [INFERRED]
  docs/ARCHITECTURE.md → crates/app/src/markdown.rs

## Import Cycles
- 1-file cycle: `crates/gpu/src/renderer.rs -> crates/gpu/src/renderer.rs`
- 2-file cycle: `crates/core/src/lib.rs -> crates/core/src/selection.rs -> crates/core/src/lib.rs`
- 2-file cycle: `crates/gpu/src/lib.rs -> crates/gpu/src/renderer.rs -> crates/gpu/src/lib.rs`
- 3-file cycle: `crates/core/src/history.rs -> crates/core/src/selection.rs -> crates/core/src/lib.rs -> crates/core/src/history.rs`

## Hyperedges (group relationships)
- **The six non-negotiable performance invariants** — docs_architecture_redraw_only_on_damage, docs_architecture_viewport_bounded_layout, docs_architecture_no_index_of_our_own, docs_architecture_one_draw_call, docs_architecture_lazy_language_servers, docs_architecture_nothing_loaded_until_opened [EXTRACTED 1.00]
- **A tab is not always a buffer: the tab model** — docs_architecture_workspace_tabs, readme_tabs, docs_architecture_diff_tab_ownership, docs_architecture_commit_diff_tab, docs_architecture_active_buffer_tab_check, docs_architecture_page_hit_testing, readme_preview_tab [INFERRED 0.85]
- **One Markdown reader serving preview, hover and fences** — readme_markdown_reader, readme_preview_system, docs_architecture_hover_contentformat, docs_architecture_fenced_code_syntax, docs_architecture_run_boundary_wrapping, docs_architecture_preview_glyph_sizes, docs_architecture_html_void_elements [INFERRED 0.85]

## Communities (88 total, 20 thin omitted)

### Community 0 - "Tab and Diff Workspace Model"
Cohesion: 0.06
Nodes (27): row_line(), a_panel_tab_holds_no_buffer_and_is_skipped_by_the_file_list(), a_pinned_tab_is_kept_when_the_next_preview_opens(), a_preview_tab_is_replaced_rather_than_added(), a_single_buffer_does_not_cycle(), a_tab_strip_says_which_tabs_are_not_files(), closing_a_diff_tab_closes_the_file_with_it(), closing_keeps_the_index_in_range() (+19 more)

### Community 1 - "App Event Loop Tests"
Cohesion: 0.05
Nodes (34): a_diff_row_names_the_working_copy_line_it_shows(), a_long_pause_is_due_immediately_rather_than_overdue(), a_menu_label_too_long_for_the_menu_is_cut(), a_signature_keeps_the_shape_the_popup_draws_it_in(), a_workspace_edit_is_read_in_both_of_its_shapes(), accepting_replaces_the_word_rather_than_following_it(), at(), autosave_delay() (+26 more)

### Community 2 - "Language Registry and Parsing"
Cohesion: 0.06
Nodes (35): Compiled, every_grammar_and_query_compiles(), every_language_captures_the_basics(), language_for_path(), LanguageConfig, LANGUAGES, an_incremental_edit_keeps_the_tree_correct(), input_edit() (+27 more)

### Community 3 - "Layout and Hit Testing"
Cohesion: 0.07
Nodes (42): a_hover_with_room_above_it_sits_above_the_character(), a_long_hover_is_capped_rather_than_filling_the_window(), a_short_file_grows_no_scrollbar(), a_tabbed_line_puts_the_caret_on_the_tab_stop(), caret_at(), caret_point(), char_col_at_visual(), Chrome (+34 more)

### Community 4 - "Command Palette Vocabulary"
Cohesion: 0.04
Nodes (48): Command, AddCursorAbove, AddCursorBelow, .ALL, BlameLine, CloseBuffer, Commit, Copy (+40 more)

### Community 6 - "Markdown Reader Tests"
Cohesion: 0.09
Nodes (41): a_closing_run_of_hashes_is_not_part_of_the_title(), a_fence_is_highlighted_when_it_names_a_language(), a_fence_suspends_all_markup(), a_fenced_block_is_tinted_including_its_padding(), a_generic_in_prose_is_left_alone(), a_hard_break_ends_the_line_without_ending_the_paragraph(), a_run_boundary_is_not_a_word_boundary(), a_sentence_with_pipes_in_it_is_not_a_table() (+33 more)

### Community 7 - "Source Control Panel"
Cohesion: 0.10
Nodes (27): a_clean_tree_says_so_instead_of_showing_empty_headings(), a_file_on_both_sides_appears_in_both_sections(), an_empty_history_says_so(), change(), GitPanel, .HISTORY, headers(), history_mode_lists_commits_instead_of_changes() (+19 more)

### Community 8 - "File Tree and Inline Naming"
Cohesion: 0.17
Nodes (22): a_name_cannot_escape_the_directory_it_was_typed_in(), a_name_that_is_taken_is_refused_rather_than_overwriting(), a_new_file_is_created_and_reported(), a_new_file_is_typed_at_a_row_inside_its_directory(), a_new_folder_is_created_expanded(), an_empty_name_makes_nothing(), cancelling_leaves_the_tree_as_it_was(), children() (+14 more)

### Community 9 - "Unified Diff Construction"
Cohesion: 0.08
Nodes (26): a_change_is_shown_as_a_removal_then_an_addition(), a_new_file_is_all_additions(), a_new_file_is_entirely_added(), a_pure_deletion_has_an_empty_right_column(), a_replacement_lines_up_on_one_row(), added_lines_are_reported_where_they_land(), both_sides_carry_their_own_line_numbers(), Change (+18 more)

### Community 10 - "Fuzzy Picker"
Cohesion: 0.15
Nodes (18): a_query_that_matches_nothing_yields_nothing(), Action, OpenPath, Run, SwitchBuffer, an_empty_query_lists_everything(), choosing_a_file_yields_its_absolute_path(), Entry (+10 more)

### Community 12 - "Editor Edit Tests"
Cohesion: 0.13
Nodes (22): a_cursor_added_below_keeps_its_column_and_clamps_on_a_short_line(), a_cursor_is_not_added_past_either_end_of_the_file(), a_selection_ending_at_a_line_start_has_not_reached_into_it(), a_whole_formatting_pass_is_one_undo(), an_edit_past_the_end_is_dropped_rather_than_panicking(), an_edit_that_changes_nothing_is_not_an_edit(), consecutive_typing_is_one_undo_step(), copying_several_cursors_joins_their_text() (+14 more)

### Community 13 - "Settings Persistence"
Cohesion: 0.10
Nodes (14): a_file_with_no_preview_never_previews(), a_written_file_round_trips_through_the_defaults(), diffs_default_to_two_columns(), each_kind_switches_independently(), only_what_was_chosen_is_written_down(), PreviewKind, .ALL, Image (+6 more)

### Community 14 - "Context Menu Actions"
Cohesion: 0.06
Nodes (32): Action, BlameLine, Calls, CodeActions, Copy, CopyAndTrim, CopyFile, CopyPath (+24 more)

### Community 15 - "Cursors and Selection"
Cohesion: 0.15
Nodes (10): buf(), collapsing_a_selection_moves_to_its_edge(), Cursor, disjoint_cursors_are_kept_and_sorted(), motion_clamps_at_both_ends(), move_head(), overlapping_cursors_merge(), Selection (+2 more)

### Community 16 - "Git Write Operations"
Cohesion: 0.19
Nodes (21): a_clean_tree_has_no_changes(), a_commits_diff_comes_back_as_lines(), a_failing_commit_reports_gits_own_words(), a_file_both_staged_and_edited_shows_on_both_sides(), a_files_log_holds_only_the_commits_that_touched_it(), a_path_outside_the_repository_is_rejected(), a_path_with_a_space_survives_the_nul_separated_parse(), a_renamed_file_keeps_the_history_it_had_before() (+13 more)

### Community 17 - "Recent Projects"
Cohesion: 0.10
Nodes (11): LIMIT, missing_directories_are_not_offered(), Recents, store_path(), the_list_is_capped(), the_store_lives_somewhere_sensible(), candidates_are_offered_in_preference_order(), config_for() (+3 more)

### Community 18 - "Buffer and Project Search"
Cohesion: 0.15
Nodes (18): buf(), highlighting_is_limited_to_the_requested_range(), Hit, literals_are_escaped_rather_than_interpreted(), matches_in_range(), next_match(), next_wraps_around_the_buffer(), Pattern (+10 more)

### Community 19 - "Git Repository Reads"
Cohesion: 0.15
Nodes (6): Commit, GitError, parse_unified(), remote_link(), Repo, this_repository_is_discovered_if_there_is_one()

### Community 21 - "Markdown Inline Scanner"
Cohesion: 0.10
Nodes (27): attribute(), bracketed(), char_byte(), decode(), find_from(), highlight_fence(), inline(), inline_tag() (+19 more)

### Community 22 - "Editor Core"
Cohesion: 0.10
Nodes (5): Editor, running_out_of_matches_wraps_rather_than_stopping(), scrolling_follows_the_cursor_both_ways(), selections(), text()

### Community 23 - "wgpu Renderer Setup"
Cohesion: 0.13
Nodes (5): Renderer, request_gpu(), Target, Offscreen, Window

### Community 24 - "Status Bar Icons"
Cohesion: 0.12
Nodes (21): draw(), every_icon_actually_has_something_in_it(), every_icon_is_a_nine_by_nine_grid(), Icon, Eye, Files, Git, Search (+13 more)

### Community 25 - "Hit Regions"
Cohesion: 0.12
Nodes (26): a_full_area_page_leaves_the_chrome_clickable(), a_tab_separates_its_body_from_its_close_box(), Hit, Context, DiffText, Elsewhere, GitCommit, GitInit (+18 more)

### Community 26 - "Glyph Atlas"
Cohesion: 0.15
Nodes (6): Atlas, CellMetrics, Size, Body, Huge, Large

### Community 27 - "LSP Protocol Framing"
Cohesion: 0.12
Nodes (11): classify(), diagnostics_are_decoded_with_their_path(), framing_reads_one_message(), from_position(), positions_round_trip(), positions_round_trip_through_wide_characters(), range_to_chars(), read_message() (+3 more)

### Community 28 - "Drawing Entry Points"
Cohesion: 0.26
Nodes (19): context_bounds(), draw(), draw_completion(), draw_context_menu(), draw_editor(), draw_find_bar(), draw_hover(), draw_image() (+11 more)

### Community 30 - "Pending LSP Replies"
Cohesion: 0.11
Nodes (13): Awaiting, Calls, CodeActions, Completion, Definition, Formatting, GoTo, Hover (+5 more)

### Community 31 - "Live Language Server Tests"
Cohesion: 0.21
Nodes (12): a_dot_comes_back_with_the_members(), a_real_server_initialises_and_reports_diagnostics(), a_real_server_names_its_completion_triggers(), a_rename_comes_back_as_a_workspace_edit(), ask_a_real_server(), formatting_comes_back_as_edits_we_can_apply(), go_to_definition_answers_against_a_real_server(), hovering_a_name_describes_it() (+4 more)

### Community 32 - "Context Menu Model"
Cohesion: 0.16
Nodes (10): a_code_action_menu_is_built_from_what_the_server_said(), CodeAction, ContextMenu, .MAX_COLUMNS, cut_and_copy_appear_only_with_a_selection(), Entry, git_entries_appear_only_inside_a_repository(), moving_the_highlight_steps_over_separators_and_wraps() (+2 more)

### Community 33 - "Text Buffer Tests"
Cohesion: 0.17
Nodes (9): a_double_click_takes_the_run_it_lands_in(), a_triple_click_takes_the_whole_line(), apply_then_revert_round_trips_multi_cursor_edits(), line_is_bounds_checked(), line_length_excludes_the_line_ending(), reads_from_a_reader(), revert_handles_mixed_length_changes(), word_motion_crosses_runs_and_whitespace() (+1 more)

### Community 34 - "Find Bar and Prompt"
Cohesion: 0.13
Nodes (6): MAX_HITS, Prompt, PromptAction, Commit, RenameSymbol, MAX_SYNCED_BYTES

### Community 35 - "Server Process Lifecycle"
Cohesion: 0.12
Nodes (9): Event, Diagnostics, Exited, Failed, Response, spawn_logger(), spawn_reader(), spawn_server() (+1 more)

### Community 36 - "Markdown Block Layout"
Cohesion: 0.16
Nodes (13): code_block(), flush(), hard_wrap(), heading_scale(), Line, push_heading(), push_run(), Scale (+5 more)

### Community 37 - "Landing Page"
Cohesion: 0.15
Nodes (17): landing_rows(), LandingAction, NewFile, NewWindow, OpenFile, OpenFolder, OpenRecent, LandingRow (+9 more)

### Community 39 - "Image Decoding and Fitting"
Cohesion: 0.20
Nodes (12): a_large_image_is_scaled_down_to_fit(), a_small_image_is_left_at_its_own_size(), extension(), fit(), ImageView, is_image(), is_svg(), load() (+4 more)

### Community 40 - "Theme Colours and Diff Drawing"
Cohesion: 0.24
Nodes (11): SyntaxColors, Theme, caret_column(), draw_diff(), draw_diff_split(), probe_scene(), Scene, scene_for() (+3 more)

### Community 41 - "Settings Page Rows"
Cohesion: 0.13
Nodes (16): ButtonSlot, Setting, Autosave, Preview, setting_rows(), SideBySideDiff, SettingRow, Blank (+8 more)

### Community 43 - "Undo History"
Cohesion: 0.23
Nodes (3): Change, History, Transaction

### Community 44 - "Git Blame"
Cohesion: 0.19
Nodes (9): a_label_reads_as_one_line(), a_line_in_no_commit_is_marked_rather_than_attributed(), BlameCommit, content_that_looks_like_a_header_does_not_become_one(), each_line_gets_its_commit_and_each_commit_is_stored_once(), empty_output_is_an_empty_blame_not_a_panic(), FileBlame, parse_blame() (+1 more)

### Community 46 - "Diff Geometry and Scene"
Cohesion: 0.21
Nodes (13): a_click_in_an_editable_diff_lands_on_the_text_it_looks_like(), a_read_only_diff_takes_no_caret_at_all(), clicking_a_status_button_hits_the_one_it_looks_like(), diff_frame(), DiffFrame, Panels, probe_panels(), probe_panels_with() (+5 more)

### Community 47 - "App Icon Generator"
Cohesion: 0.22
Nodes (4): draw(), main(), rounded_mask(), write_png()

### Community 48 - "GPU Frame Batching"
Cohesion: 0.17
Nodes (5): Batch, Globals, Image, INITIAL_QUAD_CAPACITY, Quad

### Community 49 - "Whole-Line Commands"
Cohesion: 0.20
Nodes (6): a_line_does_not_move_off_either_end(), moving_a_line_swaps_it_with_its_neighbour(), moving_onto_a_last_line_with_no_newline_keeps_the_file_whole(), selecting_the_line_takes_the_newline_with_it(), shift(), the_caret_travels_with_the_line_it_moves()

### Community 50 - "Indentation and Line Opening"
Cohesion: 0.17
Nodes (5): backspace_eats_a_whole_indent_level(), newline_carries_indentation_and_opens_a_block(), opening_a_line_keeps_the_indentation_and_moves_there(), tab_goes_to_the_next_stop(), unindent_removes_at_most_one_level()

### Community 51 - "Cursor Motions"
Cohesion: 0.17
Nodes (11): Motion, DocEnd, DocStart, Down, Left, LineEnd, LineStart, Right (+3 more)

### Community 52 - "Colour and Frame Drawing"
Cohesion: 0.21
Nodes (4): dim(), Rgba, .BLACK, .TRANSPARENT

### Community 53 - "Font Faces and Glyph Sizes"
Cohesion: 0.18
Nodes (5): Face, Bold, Regular, Glyph atlas, swash glyph rasterization

### Community 54 - "LSP Request Kinds"
Cohesion: 0.17
Nodes (12): Pending, CallHierarchy, Calls, CodeActions, Completion, Definition, Formatting, Hover (+4 more)

### Community 55 - "Native Menu Bar"
Cohesion: 0.20
Nodes (3): build(), item(), ITEMS

### Community 56 - "Explorer Context Menu"
Cohesion: 0.36
Nodes (7): a_folder_is_not_offered_a_preview(), quiet(), State, the_explorer_menu_offers_everything_it_is_meant_to(), the_menu_is_wide_enough_for_its_widest_entry(), the_root_listing_offers_only_what_it_can_do(), what_cannot_be_done_is_not_offered()

### Community 57 - "Find Query State"
Cohesion: 0.22
Nodes (4): Field, Query, Replace, Find

### Community 58 - "File Operations Undo"
Cohesion: 0.22
Nodes (8): a_free_name_steps_past_what_is_there(), FileOp, Added, Moved, Removed, free_name(), move_to_trash(), undoing_a_move_and_redoing_it_lands_where_it_started()

### Community 59 - "App Startup and Wake"
Cohesion: 0.20
Nodes (3): main(), snapshot(), Wake

### Community 60 - "Timers and Idle CPU"
Cohesion: 0.24
Nodes (4): BLINK_FOR, HOVER_DWELL, LITHE_TRACE event logging, --snapshot frame capture

### Community 63 - "Comment Toggling"
Cohesion: 0.33
Nodes (6): a_blank_line_inside_a_block_is_not_commented(), a_fully_commented_block_uncomments(), a_half_commented_block_is_commented_rather_than_inverted(), commenting_knows_which_marker_the_language_uses(), commenting_lines_up_at_the_shallowest_indent(), rust()

### Community 65 - "Preview and Hover Drawing"
Cohesion: 0.28
Nodes (7): draw_preview(), Hover, .COLUMNS, .ROWS, preview_columns(), preview_rows(), span_style()

### Community 66 - "Crate Layout"
Cohesion: 0.47
Nodes (7): lithe, lithe-core, lithe-gpu, lithe-lsp, lithe-search, lithe-syntax, lithe-vcs

### Community 70 - "Atlas Constants"
Cohesion: 0.33
Nodes (5): ATLAS_SIZE, Glyph, MONOSPACE_PREFERENCES, PAD, UvRect

### Community 71 - "Go-To Request Kinds"
Cohesion: 0.29
Nodes (5): Where, Declaration, Definition, Implementation, TypeDefinition

### Community 72 - "Request Outcome States"
Cohesion: 0.29
Nodes (5): Asked, NotInstalled, Sent, Starting, Unsupported

### Community 73 - "Diff Line Kinds"
Cohesion: 0.29
Nodes (7): line(), kinds(), LineKind, Added, Context, Removed, Skipped

### Community 74 - "Markdown Block Decoration"
Cohesion: 0.29
Nodes (7): Block, Code, HeadingRule, Plain, Quote, Rule, TableHead

### Community 75 - "Markdown Tables"
Cohesion: 0.43
Nodes (3): clip(), Table, width_of()

### Community 76 - "Project Scope and Milestones"
Cohesion: 0.29
Nodes (3): Milestones M1-M7, lithe, Roadmap M1-M7 (all done)

### Community 78 - "Diagnostic Severity"
Cohesion: 0.33
Nodes (6): Mark, Severity, Error, Hint, Information, Warning

### Community 81 - "Discard Confirmation"
Cohesion: 0.40
Nodes (5): confirm_discard(), Decision, Cancel, Discard, Save

### Community 82 - "Markdown Render Tests"
Cohesion: 0.40
Nodes (5): a_pipe_table_becomes_an_aligned_grid(), heading_lines(), long_paragraphs_wrap_at_word_boundaries(), nested_lists_change_bullet_with_depth(), text_of()

### Community 86 - "Character Classes"
Cohesion: 0.50
Nodes (4): Class, Punctuation, Space, Word

## Knowledge Gaps
- **232 isolated node(s):** `NewFile`, `NewFolder`, `Duplicate`, `Rename`, `Trash` (+227 more)
  These have ≤1 connection - possible missing edges. (Counts symbols only; 494 node(s) total have ≤1 connection when file, concept and rationale nodes are included.)
- **20 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Pane` connect `Pane Drawing and Refresh` to `Tab and Diff Workspace Model`, `App Event Loop Tests`, `Layout and Hit Testing`, `App Commands and LSP Requests`, `Source Control Panel`, `File Tree and Inline Naming`, `Unified Diff Construction`, `Fuzzy Picker`, `Window and Input Routing`, `Settings Persistence`, `Git Repository Reads`, `wgpu Renderer Setup`, `Context Menu Model`, `Find Bar and Prompt`, `Markdown Block Layout`, `Image Decoding and Fitting`, `Git Blame`, `Find Query State`, `App Startup and Wake`, `Project Search Worker`, `Preview and Hover Drawing`, `Completion Popup`, `Diagnostic Severity`?**
  _High betweenness centrality (0.128) - this node is a cross-community bridge._
- **What connects `NewFile`, `NewFolder`, `Duplicate` to the rest of the system?**
  _232 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Tab and Diff Workspace Model` be split into smaller, more focused modules?**
  _Cohesion score 0.05921325051759834 - nodes in this community are weakly interconnected._
- **Why does `Editor` connect `Editor Core` to `Tab and Diff Workspace Model`, `App Event Loop Tests`, `Language Registry and Parsing`, `Layout and Hit Testing`, `Theme Colours and Diff Drawing`, `Undo History`, `Editor Edit Tests`, `Performance Invariants`, `Cursors and Selection`, `Whole-Line Commands`, `Indentation and Line Opening`, `Cursor Motions`, `Drawing Entry Points`, `Rope Text Buffer`, `Comment Toggling`?**
  _High betweenness centrality (0.093) - this node is a cross-community bridge._
- **Should `App Event Loop Tests` be split into smaller, more focused modules?**
  _Cohesion score 0.047619047619047616 - nodes in this community are weakly interconnected._
- **Why does `App` connect `App Commands and LSP Requests` to `App Event Loop Tests`, `Theme Colours and Diff Drawing`, `Language Server Manager`, `Window and Input Routing`, `Settings Persistence`, `Recent Projects`, `Pane Drawing and Refresh`, `File Operations Undo`, `App Startup and Wake`?**
  _High betweenness centrality (0.064) - this node is a cross-community bridge._
- **Should `Language Registry and Parsing` be split into smaller, more focused modules?**
  _Cohesion score 0.055051421657592255 - nodes in this community are weakly interconnected._