# Graph Report - bread  (2026-08-16)

## Corpus Check
- 66 files · ~90,429 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 1486 nodes · 3384 edges · 93 communities (68 shown, 25 thin omitted)
- Extraction: 99% EXTRACTED · 1% INFERRED · 0% AMBIGUOUS · INFERRED: 47 edges (avg confidence: 0.78)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `cdd5de8f`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- LuaEngine
- RawEvent
- config.rs
- Server
- widget.rs
- Bread Daemon (breadd)
- filesystem.rs
- git.rs
- Result
- state_engine.rs
- modules_mgmt.rs
- systemd.rs
- hooks_git.rs
- bread-cli/src/main.rs
- types.rs
- podman.rs
- Value
- Result
- hooks_shell.rs
- bluetooth.rs
- SubscriptionId
- glob.rs
- StateHandle
- Adapter
- network.rs
- power.rs
- hyprland.rs
- parse_upower_message
- main Branch
- git-branch-widget.lua
- TestHarness
- Sync
- active-window-widget.lua
- .new
- focus-mode-widget.lua
- workflow-status-widget.lua
- bread.widget API
- bluetooth-toggle-widget.lua
- pause-media-on-headphone-unplug.lua
- Autostart Example Module
- install.sh
- Filesystem Adapter
- Git Adapter
- Podman Adapter
- Systemd Adapter
- bread.after(delay_ms, fn)
- bread.bluetooth namespace
- bread.every(interval_ms, fn)
- bread.exec(cmd)
- bread.hyprland namespace
- bread.notify(message, opts)
- bread.state.watch(path, fn)
- bread.system.startup
- Monitors Configuration Example
- Binds Module (Built-in)
- lua/mod.rs
- external-monitors.lua
- AGENTS.md — Repo hygiene
- CLAUDE.md — Repo hygiene
- ModuleHostLua
- ModuleHostRegistry
- xtask/src/main.rs
- Bread
- rules.rs
- Normalized events
- Bread Documentation
- udev.rs
- Dictionary: Lua API
- Out-of-process module sandboxing *(Since: v1.6)*
- Dictionary: Built-in modules
- Events
- Machine and filesystem
- Contributing
- Bluetooth
- Widgets *(Since: v1.3)*
- Getting started
- Capability-scoped modules *(Since: v1.5)*
- Workflows *(Since: v1.2)*
- Timers
- State
- init.lua
- Execution
- packaging/README.md
- PathBuf
- build.sh
- beta Release Track
- dev Release Track
- stable Release Track

## God Nodes (most connected - your core abstractions)
1. `LuaEngine` - 59 edges
2. `RawEvent` - 50 edges
3. `BreadEvent` - 38 edges
4. `raw()` - 37 edges
5. `RuntimeState` - 36 edges
6. `StateHandle` - 28 edges
7. `now_unix_ms()` - 25 edges
8. `Adapter` - 25 edges
9. `SubscriptionId` - 25 edges
10. `ModuleHostLua` - 24 edges

## Surprising Connections (you probably didn't know these)
- `parse_bluetooth_message()` --calls--> `now_unix_ms()`  [INFERRED]
  breadd/src/adapters/bluetooth.rs → bread-shared/src/lib.rs
- `try_enumerate()` --calls--> `now_unix_ms()`  [INFERRED]
  breadd/src/adapters/bluetooth.rs → bread-shared/src/lib.rs
- `classify()` --calls--> `now_unix_ms()`  [INFERRED]
  breadd/src/adapters/filesystem.rs → bread-shared/src/lib.rs
- `emit_topology_snapshot()` --calls--> `now_unix_ms()`  [INFERRED]
  breadd/src/adapters/hyprland.rs → bread-shared/src/lib.rs
- `network_raw_event()` --calls--> `now_unix_ms()`  [INFERRED]
  breadd/src/adapters/network.rs → bread-shared/src/lib.rs

## Import Cycles
- 2-file cycle: `breadd/src/core/state_engine.rs -> breadd/src/lua/mod.rs -> breadd/src/core/state_engine.rs`
- 2-file cycle: `bread-shared/src/lib.rs -> bread-shared/src/module_host_ipc.rs -> bread-shared/src/lib.rs`

## Hyperedges (group relationships)
- **** — adapter_udev, adapter_hyprland, adapter_power, sys_bread_daemon, api_bread_on, sys_lua_runtime [INFERRED]
- **** — config_init_lua, config_modules_dir, pattern_module_skeleton, api_bread_on, sys_lua_runtime [INFERRED]
- **** — api_bread_workflow, api_bread_spawn, api_bread_wait, example_dock_workflow, api_bread_notify [INFERRED]
- **** — api_bread_widget, api_bread_every, example_widget_cpu_temp, api_bread_state_watch [INFERRED]

## Communities (93 total, 25 thin omitted)

### Community 0 - "LuaEngine"
Cohesion: 0.11
Nodes (19): ErrorEntry, HandlerEntry, HandlerKind, LuaEngine, LuaMessage, ModuleInfo, AtomicU64, HashMap (+11 more)

### Community 1 - "RawEvent"
Cohesion: 0.06
Nodes (67): adapter_source_is_hashable_and_eq(), AdapterSource, bread_event_new_accepts_owned_and_borrowed_names(), bread_event_new_assigns_unique_id_and_no_cause(), bread_event_new_sets_current_timestamp(), bread_event_with_timestamp_preserves_timestamp_and_assigns_id(), BreadEvent, DaemonSection (+59 more)

### Community 2 - "config.rs"
Cohesion: 0.07
Nodes (50): AdaptersConfig, AdapterToggle, compat_section_defaults_legacy_hyprland_names_to_true(), CompatConfig, Config, config_path(), config_path_falls_back_to_home_when_no_xdg(), config_path_respects_xdg_config_home() (+42 more)

### Community 3 - "Server"
Cohesion: 0.05
Nodes (41): A, command_target(), event_domain(), is_known_app(), is_reserved_domain(), Option, validate_app_namespace(), validate_command_event() (+33 more)

### Community 4 - "widget.rs"
Cohesion: 0.07
Nodes (33): accepts_node_count_at_max(), accepts_tree_at_max_depth(), Align, Background, box_of(), default_orientation(), FontWeight, is_valid_class() (+25 more)

### Community 5 - "Bread Daemon (breadd)"
Cohesion: 0.06
Nodes (36): Bluetooth Adapter, Hyprland Adapter, Network Adapter, Power Adapter, udev Adapter, bread.on(pattern, fn), bread.spawn(fn), bread.wait(pattern, opts) (+28 more)

### Community 6 - "filesystem.rs"
Cohesion: 0.12
Nodes (28): classify(), classify_build_artifact_created_in_target(), classify_debounces_rapid_repeat_events_for_same_path(), classify_file_changed_for_ordinary_source_file(), classify_silent_for_modify_in_target_not_create(), classify_silent_under_git(), classify_silent_under_node_modules(), detect_markers() (+20 more)

### Community 7 - "git.rs"
Cohesion: 0.12
Nodes (22): check_ahead_behind(), check_dirty(), discover_repos(), expand_roots(), expand_roots_globs_single_trailing_star(), expand_roots_skips_unreadable_glob_parent_without_panicking(), expand_roots_uses_literal_path_without_trailing_star(), GitAdapter (+14 more)

### Community 8 - "Result"
Cohesion: 0.15
Nodes (50): daemon_survives_repeated_reloads_and_pipeline_resumes(), emit_with_app_source_allows_command_to_another_app(), emit_with_app_source_rejects_wrong_namespace(), emit_with_app_source_still_rejects_foreign_app_namespace(), emit_with_internal_source_is_rejected(), emit_with_known_app_source_routes_through_normalizer(), emit_with_unregistered_app_source_is_rejected(), emit_without_event_errors() (+42 more)

### Community 9 - "state_engine.rs"
Cohesion: 0.12
Nodes (20): apply_device_change(), apply_event_to_state(), device_connect_adds_device_with_all_fields(), device_connect_is_idempotent_for_same_id(), device_disconnect_of_unknown_id_is_noop(), device_disconnect_removes_matching_id(), ev(), hyprland_snapshot_replaces_topology() (+12 more)

### Community 10 - "modules_mgmt.rs"
Cohesion: 0.11
Nodes (36): audit_detects_fs_read_and_widget_from_cpu_temp_widget_style_module(), audit_extracts_exec_bin_hint_and_ignores_baseline_calls(), audit_module(), audit_scans_required_sibling_files_in_module_directory(), classify_call_site(), collect_lua_files(), copy_dir(), extract_first_string_arg() (+28 more)

### Community 11 - "systemd.rs"
Cohesion: 0.12
Nodes (17): active_state_to_kind(), failure_result(), get_unit_path(), handle_message(), is_failed_transition(), query_active_state(), Connection, HashMap (+9 more)

### Community 12 - "hooks_git.rs"
Cohesion: 0.15
Nodes (24): all_hook_scripts_exit_0_unconditionally(), all_hook_scripts_start_with_shebang_and_marker(), branch_changed_emit_line(), commit_created_emit_line(), emit_line_for(), git_dir(), hook_script(), hook_script_rejects_unknown_name() (+16 more)

### Community 13 - "bread-cli/src/main.rs"
Cohesion: 0.19
Nodes (29): CausalityTracker, Cli, Commands, config_directory(), daemon_socket_path(), format_timestamp(), handle_modules_cmd(), HooksCommand (+21 more)

### Community 14 - "types.rs"
Cohesion: 0.15
Nodes (21): DeviceTopology, InterfaceState, MatchCondition, ModuleStatus, Monitor, NetworkState, PowerState, ProfileState (+13 more)

### Community 15 - "podman.rs"
Cohesion: 0.15
Nodes (17): container_event(), ignores_remove_event(), ignores_stop_event_to_avoid_double_emit_with_died(), ignores_unknown_action(), map_podman_event(), maps_died_event(), maps_health_status_event(), maps_start_event() (+9 more)

### Community 16 - "Value"
Cohesion: 0.17
Nodes (14): active_window_from_data(), apply_hyprland_snapshot(), condition_matches(), hyprland_state_key(), json_stringish(), resolve_device(), Option, Result (+6 more)

### Community 17 - "Result"
Cohesion: 0.13
Nodes (26): bluetooth_connect(), bluetooth_disconnect(), bluetooth_find_adapter(), bluetooth_get_powered(), bluetooth_query(), bluetooth_set_powered(), bluetooth_set_scanning(), bluetooth_spawn() (+18 more)

### Community 18 - "hooks_shell.rs"
Cohesion: 0.18
Nodes (11): hook_scripts_background_every_emit_call(), hooks_dir(), install_shell(), join_line_continuations(), Option, PathBuf, Result, String (+3 more)

### Community 19 - "bluetooth.rs"
Cohesion: 0.15
Nodes (10): address_from_path(), BluetoothAdapter, parse_bluetooth_message(), Message, Option, Result, Self, Sender (+2 more)

### Community 20 - "SubscriptionId"
Cohesion: 0.29
Nodes (13): HashMap, String, Vec, Subscription, SubscriptionId, SubscriptionTable, table_add_assigns_provided_id_and_finds_match(), table_clear_removes_all() (+5 more)

### Community 22 - "StateHandle"
Cohesion: 0.15
Nodes (16): dispatch_event(), handle_command(), Arc, AtomicU64, HashMap, Receiver, RwLock, Self (+8 more)

### Community 23 - "Adapter"
Cohesion: 0.19
Nodes (9): Adapter, ip_from_bytes(), Option, Result, Self, Sender, String, RtnetlinkAdapter (+1 more)

### Community 24 - "network.rs"
Cohesion: 0.27
Nodes (9): has_default_route(), network_raw_event(), NetworkAdapter, NetworkSnapshot, read_network_state(), BTreeMap, Result, Sender (+1 more)

### Community 25 - "power.rs"
Cohesion: 0.26
Nodes (8): power_raw_event(), PowerAdapter, PowerSnapshot, read_power_state(), Option, Result, Self, Sender

### Community 26 - "hyprland.rs"
Cohesion: 0.29
Nodes (11): emit_topology_snapshot(), hyprland_event_socket(), hyprland_request_json(), hyprland_request_socket(), HyprlandAdapter, parse_hyprland_line(), PathBuf, Result (+3 more)

### Community 27 - "parse_upower_message"
Cohesion: 0.27
Nodes (6): parse_upower_message(), Message, Result, Self, Sender, UPowerAdapter

### Community 29 - "git-branch-widget.lua"
Cohesion: 0.62
Nodes (6): focused_tab_cwd(), git_info(), M.on_load(), shell_quote(), update(), widget_root()

### Community 30 - "TestHarness"
Cohesion: 0.16
Nodes (16): main(), parse_args(), Option, String, killing_a_module_host_child_does_not_take_down_breadd_or_other_modules(), os_execute_and_io_open_are_denied_at_the_kernel_level_outside_granted_scope(), Child, Drop (+8 more)

### Community 31 - "Sync"
Cohesion: 0.50
Nodes (3): main(), Result, Sync

### Community 32 - "active-window-widget.lua"
Cohesion: 0.83
Nodes (3): label_for(), M.on_load(), widget_root()

### Community 33 - ".new"
Cohesion: 0.16
Nodes (15): ModulePermission, Option, String, bluetooth_list_devices(), builtin_module_decls(), is_lib_path(), list_lua_files(), module_name_from_path() (+7 more)

### Community 34 - "focus-mode-widget.lua"
Cohesion: 1.00
Nodes (3): is_focused(), M.on_load(), widget_root()

### Community 35 - "workflow-status-widget.lua"
Cohesion: 0.83
Nodes (3): M.on_load(), most_relevant(), widget_update()

### Community 36 - "bread.widget API"
Cohesion: 0.67
Nodes (3): bread.widget API, CPU Temperature Widget Example, Live Widget Update Pattern

### Community 60 - "lua/mod.rs"
Cohesion: 0.30
Nodes (20): now_unix_ms(), RuntimeState, module_store_get(), module_store_set(), Arc, JsonValue, RwLock, widget_list_json() (+12 more)

### Community 61 - "external-monitors.lua"
Cohesion: 0.29
Nodes (8): apply(), apply_monitor(), connected(), drm_first_mode(), drm_status(), is_internal(), list_connectors(), M.on_load()

### Community 62 - "AGENTS.md — Repo hygiene"
Cohesion: 0.33
Nodes (5): AGENTS.md — Repo hygiene, CI, Don't, Local architecture (still true), Remotes

### Community 63 - "CLAUDE.md — Repo hygiene"
Cohesion: 0.40
Nodes (4): CI, CLAUDE.md — Repo hygiene, Don't, Remotes

### Community 64 - "ModuleHostLua"
Cohesion: 0.09
Nodes (38): call(), HostMessage, IoCommand, RpcResponse, Duration, Option, PathBuf, Receiver (+30 more)

### Community 65 - "ModuleHostRegistry"
Cohesion: 0.07
Nodes (43): bin_allowed(), path_allowed(), HashMap, Option, OwnedWriteHalf, Result, Sender, String (+35 more)

### Community 66 - "xtask/src/main.rs"
Cohesion: 0.11
Nodes (36): BTreeSet, ExitCode, check(), CheckReport, clean_state_passes(), extract_cli_commands(), extract_enum_variants(), extract_ipc_methods() (+28 more)

### Community 67 - "Bread"
Cohesion: 0.06
Nodes (28): Deprecations, Hyprland legacy flat event names (since v1.5), Bread Examples, Example 1: Porting keyboard_and_display_watcher.sh (system script), Example 2: Porting autostart.lua, Example 3: Porting display/monitors.lua, Example 4: Multi-step automation workflows, Example 5: A live widget in breadbar (+20 more)

### Community 68 - "rules.rs"
Cohesion: 0.14
Nodes (27): empty_file_loads_with_no_rules(), empty_on_is_treated_as_missing(), invalid_toml_is_fatal(), load_rules(), missing_on_is_reported_with_index_and_no_on(), multiple_action_keys_is_reported(), one_bad_rule_does_not_block_other_valid_rules(), ParsedRule (+19 more)

### Community 69 - "Normalized events"
Cohesion: 0.12
Nodes (16): Bluetooth (BlueZ), Compatibility: `[compat]` config, Devices (udev / Bluetooth), Filesystem / project detection, Git (hooks + dirty-state poller), Hyprland, Network, Normalized events (+8 more)

### Community 70 - "Bread Documentation"
Cohesion: 0.14
Nodes (14): API Stability & Versioning, Bread Documentation, Contents, Debugging tips, Dictionary: Event reference, Dictionary: IPC protocol, Dictionary: Runtime state schema, Integrating a bread\* app (+6 more)

### Community 71 - "udev.rs"
Cohesion: 0.18
Nodes (18): build_device_event(), build_event(), enumerate_payload_includes_classification_fields(), prop_bool(), prop_str(), Option, Result, Self (+10 more)

### Community 72 - "Dictionary: Lua API"
Cohesion: 0.17
Nodes (12): `bread.debounce(delay_ms, fn) -> wrapped_fn`, `bread.log(msg)` / `bread.warn(msg)` / `bread.error(msg)`, `bread.notify(message, opts)`, `bread.profile.activate(name)`, Dictionary: Lua API, Hyprland, Module declaration, Module lifecycle hooks (+4 more)

### Community 73 - "Out-of-process module sandboxing *(Since: v1.6)*"
Cohesion: 0.20
Nodes (10): Architecture, Crash isolation, New IPC methods, Out-of-process module sandboxing *(Since: v1.6)*, RPC bridge coverage, The gap this closes, The Landlock sandbox, The token/identity handshake (+2 more)

### Community 74 - "Dictionary: Built-in modules"
Cohesion: 0.20
Nodes (10): `bread.binds`, `bread.devices`, `bread.monitors`, `bread.rules` *(Since: v1.5)*, `bread.workspaces`, Device rule options, Dictionary: Built-in modules, Example: Dock-specific setup (+2 more)

### Community 75 - "Events"
Cohesion: 0.20
Nodes (10): `bread.emit(event, data)`, `bread.filter(pattern, fn, opts) -> id`, `bread.off(id)`, `bread.on(pattern, fn) -> id`, `bread.once(pattern, fn) -> id`, `bread.spawn(fn)`, `bread.wait_all(patterns, opts) -> table` *(Since: v1.2)*, `bread.wait_any(patterns, opts) -> event | nil` *(Since: v1.2)* (+2 more)

### Community 76 - "Machine and filesystem"
Cohesion: 0.20
Nodes (10): `bread.fs.exists(path) -> bool`, `bread.fs.expand(path) -> string`, `bread.fs.read(path) -> string | nil`, `bread.fs.readlink(path) -> string | nil`, `bread.fs.write(path, content)`, `bread.json.decode(str) -> table | nil`, `bread.machine.has_tag(tag) -> bool`, `bread.machine.name() -> string` (+2 more)

### Community 77 - "Contributing"
Cohesion: 0.22
Nodes (8): Branches, CI, Contributing, Keeping the API docs honest, Local development, Questions, The release cycle, Tracks, from a user's perspective

### Community 78 - "Bluetooth"
Cohesion: 0.22
Nodes (9): Bluetooth, `bread.bluetooth.connect(address)`, `bread.bluetooth.devices() -> table | nil`, `bread.bluetooth.disconnect(address)`, `bread.bluetooth.power(enabled)`, `bread.bluetooth.powered() -> bool | nil`, `bread.bluetooth.scan(enabled)`, Example: auto-connect headphones on AC power (+1 more)

### Community 79 - "Widgets *(Since: v1.3)*"
Cohesion: 0.22
Nodes (9): `bread.widget.list() -> table`, `bread.widget.register(spec) -> ok, err`, `bread.widget.remove(id) -> bool`, `bread.widget.update(id, patch) -> ok, err`, Click events, Node types, `style` *(Since: v1.4)*, Style vs. `class` (+1 more)

### Community 80 - "Getting started"
Cohesion: 0.33
Nodes (6): 1) Create a minimal config, 2) The fast path: `rules.toml` *(Since: v1.5)*, 3) Minimal `init.lua`, 4) Start the daemon, 5) Check that it's running, Getting started

### Community 81 - "Capability-scoped modules *(Since: v1.5)*"
Cohesion: 0.33
Nodes (6): Baseline (always available, no manifest entry needed), `bread modules audit <name>`, Capability-scoped modules *(Since: v1.5)*, Gated — requires a matching `[[permissions]]` entry, `path`/`bin` enforcement depends on where the module runs, `require("bread.devices")` still works from a scoped module

### Community 82 - "Workflows *(Since: v1.2)*"
Cohesion: 0.33
Nodes (6): `bread.workflow.define(name, fn)`, `bread.workflow.list() -> table`, `bread.workflow.start(name, opts)`, `bread.workflow.status(name) -> table | nil`, `bread.workflow.step(label)`, Workflows *(Since: v1.2)*

### Community 83 - "Timers"
Cohesion: 0.50
Nodes (4): `bread.after(delay_ms, fn) -> id`, `bread.cancel(id)`, `bread.every(interval_ms, fn) -> id`, Timers

### Community 84 - "State"
Cohesion: 0.50
Nodes (4): `bread.state.get(path)`, `bread.state.watch(path, fn) -> id`, State, Typed shorthands

### Community 85 - "init.lua"
Cohesion: 0.83
Nodes (3): M.on_load(), read_temp_c(), widget_root()

### Community 86 - "Execution"
Cohesion: 0.67
Nodes (3): `bread.exec_capture(cmd, opts) -> ok, stdout`, `bread.exec(cmd)`, Execution

### Community 88 - "PathBuf"
Cohesion: 1.00
Nodes (3): dirs_home(), lua_expand_path(), PathBuf

## Knowledge Gaps
- **182 isolated node(s):** `build.sh script`, `install.sh script`, `Remotes`, `CI`, `Local architecture (still true)` (+177 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **25 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Adapter` connect `Adapter` to `Server`, `filesystem.rs`, `git.rs`, `udev.rs`, `systemd.rs`, `podman.rs`, `bluetooth.rs`, `network.rs`, `power.rs`, `hyprland.rs`, `parse_upower_message`, `Sync`?**
  _High betweenness centrality (0.089) - this node is a cross-community bridge._
- **Why does `RawEvent` connect `RawEvent` to `Server`, `filesystem.rs`, `git.rs`, `udev.rs`, `systemd.rs`, `podman.rs`, `bluetooth.rs`, `Adapter`, `network.rs`, `power.rs`, `hyprland.rs`, `parse_upower_message`?**
  _High betweenness centrality (0.084) - this node is a cross-community bridge._
- **Why does `BreadEvent` connect `RawEvent` to `ModuleHostLua`, `LuaEngine`, `.new`, `Server`, `ModuleHostRegistry`, `state_engine.rs`, `StateHandle`?**
  _High betweenness centrality (0.061) - this node is a cross-community bridge._
- **What connects `build.sh script`, `install.sh script`, `Remotes` to the rest of the system?**
  _182 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `LuaEngine` be split into smaller, more focused modules?**
  _Cohesion score 0.11153846153846154 - nodes in this community are weakly interconnected._
- **Should `RawEvent` be split into smaller, more focused modules?**
  _Cohesion score 0.05702970297029703 - nodes in this community are weakly interconnected._
- **Should `config.rs` be split into smaller, more focused modules?**
  _Cohesion score 0.0670122176971492 - nodes in this community are weakly interconnected._