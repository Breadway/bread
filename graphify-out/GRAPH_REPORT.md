# Graph Report - .  (2026-08-04)

## Corpus Check
- 55 files · ~54,650 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 978 nodes · 2283 edges · 64 communities (41 shown, 23 thin omitted)
- Extraction: 99% EXTRACTED · 1% INFERRED · 0% AMBIGUOUS · INFERRED: 33 edges (avg confidence: 0.77)
- Token cost: 72,759 input · 0 output

## Community Hubs (Navigation)
- Lua Runtime Core
- Adapter Framework
- Adapter Configuration
- App Classification
- Widget Spec & Placement
- Adapter Implementations
- Filesystem Adapter
- Git State Tracking
- Integration Tests
- State Engine
- Module Management
- Systemd Adapter
- Git Hooks
- CLI Core
- Type Definitions
- Podman Adapter
- Event Routing
- Udev Adapter
- Shell Hooks
- Bluetooth Adapter
- Subscription System
- Glob Pattern Matching
- Event Dispatch
- Network RTNetlink
- Network Adapter
- Power Management
- Hyprland Integration
- UPower Integration
- CI/CD Workflows
- Git Widget
- Emit CLI Tool
- Shared Library Init
- Active Window Widget
- CPU Temp Widget
- Focus Mode Widget
- Workflow Status Widget
- Widget API Documentation
- Bluetooth Widget
- Media Pause Widget
- Module Patterns
- Install Script
- Filesystem Adapter Spec
- Git Adapter Spec
- Podman Adapter Spec
- Systemd Adapter Spec
- Lua Timer API (after)
- Lua Bluetooth API
- Lua Timer API (every)
- Lua Exec API
- Lua Hyprland API
- Lua Notify API
- Lua State Watch API
- System Startup Event
- Monitor Layout Config
- Binds Module
- Bakery Package Manager
- GUI Control Center Roadmap
- Cross-Device Mesh Roadmap
- Version Computation

## God Nodes (most connected - your core abstractions)
1. `LuaEngine` - 52 edges
2. `RawEvent` - 47 edges
3. `RuntimeState` - 34 edges
4. `BreadEvent` - 30 edges
5. `raw()` - 30 edges
6. `StateHandle` - 27 edges
7. `now_unix_ms()` - 25 edges
8. `Adapter` - 25 edges
9. `SubscriptionId` - 25 edges
10. `Server` - 22 edges

## Surprising Connections (you probably didn't know these)
- `parse_bluetooth_message()` --calls--> `now_unix_ms()`  [INFERRED]
  breadd/src/adapters/bluetooth.rs → bread-shared/src/lib.rs
- `try_enumerate()` --calls--> `now_unix_ms()`  [INFERRED]
  breadd/src/adapters/bluetooth.rs → bread-shared/src/lib.rs
- `classify()` --calls--> `now_unix_ms()`  [INFERRED]
  breadd/src/adapters/filesystem.rs → bread-shared/src/lib.rs
- `network_raw_event()` --calls--> `now_unix_ms()`  [INFERRED]
  breadd/src/adapters/network.rs → bread-shared/src/lib.rs
- `power_raw_event()` --calls--> `now_unix_ms()`  [INFERRED]
  breadd/src/adapters/power.rs → bread-shared/src/lib.rs

## Import Cycles
- 2-file cycle: `breadd/src/core/state_engine.rs -> breadd/src/lua/mod.rs -> breadd/src/core/state_engine.rs`

## Hyperedges (group relationships)
- **** — ci_dev_release, workflow_version_compute, release_track_dev, distribution_dl_breadway_dev, package_bakery [INFERRED]
- **** — adapter_udev, adapter_hyprland, adapter_power, sys_bread_daemon, api_bread_on, sys_lua_runtime [INFERRED]
- **** — config_init_lua, config_modules_dir, pattern_module_skeleton, api_bread_on, sys_lua_runtime [INFERRED]
- **** — api_bread_workflow, api_bread_spawn, api_bread_wait, example_dock_workflow, api_bread_notify [INFERRED]
- **** — api_bread_widget, api_bread_every, example_widget_cpu_temp, api_bread_state_watch [INFERRED]
- **** — branch_main, ci_dev_release, ci_rc_release, ci_stable_release, release_track_dev, release_track_beta, release_track_stable [INFERRED]

## Communities (64 total, 23 thin omitted)

### Community 0 - "Lua Runtime Core"
Cohesion: 0.06
Nodes (80): now_unix_ms(), WidgetPlacement, WidgetSpec, RuntimeState, bluetooth_connect(), bluetooth_disconnect(), bluetooth_find_adapter(), bluetooth_get_powered() (+72 more)

### Community 1 - "Adapter Framework"
Cohesion: 0.07
Nodes (52): adapter_source_is_hashable_and_eq(), AdapterSource, bread_event_new_accepts_owned_and_borrowed_names(), bread_event_new_sets_current_timestamp(), BreadEvent, DaemonSection, expand_path(), now_unix_ms_is_monotonically_non_decreasing_across_calls() (+44 more)

### Community 2 - "Adapter Configuration"
Cohesion: 0.07
Nodes (45): AdaptersConfig, AdapterToggle, Config, config_path(), config_path_falls_back_to_home_when_no_xdg(), config_path_respects_xdg_config_home(), DaemonConfig, default_config_uses_documented_defaults() (+37 more)

### Community 3 - "App Classification"
Cohesion: 0.06
Nodes (36): A, is_known_app(), validate_app_namespace(), AdapterStatus, Manager, Arc, HashMap, Receiver (+28 more)

### Community 4 - "Widget Spec & Placement"
Cohesion: 0.07
Nodes (31): accepts_node_count_at_max(), accepts_tree_at_max_depth(), Align, Background, box_of(), default_orientation(), FontWeight, is_valid_class() (+23 more)

### Community 5 - "Adapter Implementations"
Cohesion: 0.06
Nodes (39): Bluetooth Adapter, Hyprland Adapter, Network Adapter, Power Adapter, udev Adapter, bread.on(pattern, fn), bread.spawn(fn), bread.wait(pattern, opts) (+31 more)

### Community 6 - "Filesystem Adapter"
Cohesion: 0.12
Nodes (28): classify(), classify_build_artifact_created_in_target(), classify_debounces_rapid_repeat_events_for_same_path(), classify_file_changed_for_ordinary_source_file(), classify_silent_for_modify_in_target_not_create(), classify_silent_under_git(), classify_silent_under_node_modules(), detect_markers() (+20 more)

### Community 7 - "Git State Tracking"
Cohesion: 0.12
Nodes (22): check_ahead_behind(), check_dirty(), discover_repos(), expand_roots(), expand_roots_globs_single_trailing_star(), expand_roots_skips_unreadable_glob_parent_without_panicking(), expand_roots_uses_literal_path_without_trailing_star(), GitAdapter (+14 more)

### Community 8 - "Integration Tests"
Cohesion: 0.24
Nodes (30): daemon_survives_repeated_reloads_and_pipeline_resumes(), emit_with_app_source_rejects_wrong_namespace(), emit_with_internal_source_is_rejected(), emit_with_known_app_source_routes_through_normalizer(), emit_with_unregistered_app_source_is_rejected(), emit_without_event_errors(), event_stream_filter_excludes_non_matching_events(), events_replay_returns_buffered_events() (+22 more)

### Community 9 - "State Engine"
Cohesion: 0.12
Nodes (19): apply_device_change(), apply_event_to_state(), device_connect_adds_device_with_all_fields(), device_connect_is_idempotent_for_same_id(), device_disconnect_of_unknown_id_is_noop(), device_disconnect_removes_matching_id(), ev(), monitor_connect_adds_new_monitor() (+11 more)

### Community 10 - "Module Management"
Cohesion: 0.14
Nodes (24): copy_dir(), install_from_local(), list_modules(), ModuleManifest, modules_dir(), parse_source(), read_manifest_file(), read_module_manifest() (+16 more)

### Community 11 - "Systemd Adapter"
Cohesion: 0.12
Nodes (17): active_state_to_kind(), failure_result(), get_unit_path(), handle_message(), is_failed_transition(), query_active_state(), Connection, HashMap (+9 more)

### Community 12 - "Git Hooks"
Cohesion: 0.15
Nodes (24): all_hook_scripts_exit_0_unconditionally(), all_hook_scripts_start_with_shebang_and_marker(), branch_changed_emit_line(), commit_created_emit_line(), emit_line_for(), git_dir(), hook_script(), hook_script_rejects_unknown_name() (+16 more)

### Community 13 - "CLI Core"
Cohesion: 0.22
Nodes (27): Cli, Commands, config_directory(), daemon_socket_path(), format_timestamp(), handle_modules_cmd(), HooksCommand, install_module() (+19 more)

### Community 14 - "Type Definitions"
Cohesion: 0.15
Nodes (23): Device, DeviceRule, DeviceTopology, InterfaceState, MatchCondition, ModuleStatus, Monitor, NetworkState (+15 more)

### Community 15 - "Podman Adapter"
Cohesion: 0.15
Nodes (17): container_event(), ignores_remove_event(), ignores_stop_event_to_avoid_double_emit_with_died(), ignores_unknown_action(), map_podman_event(), maps_died_event(), maps_health_status_event(), maps_start_event() (+9 more)

### Community 16 - "Event Routing"
Cohesion: 0.13
Nodes (10): condition_matches(), resolve_device(), Option, Result, String, Value, Vec, StateHandle (+2 more)

### Community 17 - "Udev Adapter"
Cohesion: 0.23
Nodes (14): build_event(), enumerate_with_udev(), prop_bool(), prop_str(), Option, Result, Self, Sender (+6 more)

### Community 18 - "Shell Hooks"
Cohesion: 0.18
Nodes (11): hook_scripts_background_every_emit_call(), hooks_dir(), install_shell(), join_line_continuations(), Option, PathBuf, Result, String (+3 more)

### Community 19 - "Bluetooth Adapter"
Cohesion: 0.15
Nodes (10): address_from_path(), BluetoothAdapter, parse_bluetooth_message(), Message, Option, Result, Self, Sender (+2 more)

### Community 20 - "Subscription System"
Cohesion: 0.29
Nodes (13): HashMap, String, Vec, Subscription, SubscriptionId, SubscriptionTable, table_add_assigns_provided_id_and_finds_match(), table_clear_removes_all() (+5 more)

### Community 22 - "Event Dispatch"
Cohesion: 0.27
Nodes (12): dispatch_event(), handle_command(), Arc, AtomicU64, Receiver, RwLock, Self, Sender (+4 more)

### Community 23 - "Network RTNetlink"
Cohesion: 0.19
Nodes (9): Adapter, ip_from_bytes(), Option, Result, Self, Sender, String, RtnetlinkAdapter (+1 more)

### Community 24 - "Network Adapter"
Cohesion: 0.27
Nodes (9): has_default_route(), network_raw_event(), NetworkAdapter, NetworkSnapshot, read_network_state(), BTreeMap, Result, Sender (+1 more)

### Community 25 - "Power Management"
Cohesion: 0.26
Nodes (8): power_raw_event(), PowerAdapter, PowerSnapshot, read_power_state(), Option, Result, Self, Sender

### Community 26 - "Hyprland Integration"
Cohesion: 0.29
Nodes (7): hyprland_event_socket(), HyprlandAdapter, parse_hyprland_line(), PathBuf, Result, Sender, String

### Community 27 - "UPower Integration"
Cohesion: 0.27
Nodes (6): parse_upower_message(), Message, Result, Self, Sender, UPowerAdapter

### Community 28 - "CI/CD Workflows"
Cohesion: 0.25
Nodes (8): main Branch, dev-release.yml Workflow, rc-release.yml Workflow, release.yml Workflow, dl.breadway.dev Distribution, beta Release Track, dev Release Track, stable Release Track

### Community 29 - "Git Widget"
Cohesion: 0.62
Nodes (6): focused_tab_cwd(), git_info(), M.on_load(), shell_quote(), update(), widget_root()

### Community 30 - "Emit CLI Tool"
Cohesion: 0.40
Nodes (5): main(), parse_args(), Option, String, UnixStream

### Community 31 - "Shared Library Init"
Cohesion: 0.50
Nodes (3): main(), Result, Sync

### Community 32 - "Active Window Widget"
Cohesion: 0.83
Nodes (3): label_for(), M.on_load(), widget_root()

### Community 33 - "CPU Temp Widget"
Cohesion: 0.83
Nodes (3): M.on_load(), read_temp_c(), widget_root()

### Community 34 - "Focus Mode Widget"
Cohesion: 1.00
Nodes (3): is_focused(), M.on_load(), widget_root()

### Community 35 - "Workflow Status Widget"
Cohesion: 0.83
Nodes (3): M.on_load(), most_relevant(), widget_update()

### Community 36 - "Widget API Documentation"
Cohesion: 0.67
Nodes (3): bread.widget API, CPU Temperature Widget Example, Live Widget Update Pattern

## Knowledge Gaps
- **44 isolated node(s):** `install.sh script`, `Git Adapter`, `Filesystem Adapter`, `Systemd Adapter`, `Podman Adapter` (+39 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **23 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Adapter` connect `Network RTNetlink` to `App Classification`, `Filesystem Adapter`, `Git State Tracking`, `Systemd Adapter`, `Podman Adapter`, `Udev Adapter`, `Bluetooth Adapter`, `Network Adapter`, `Power Management`, `Hyprland Integration`, `UPower Integration`, `Shared Library Init`?**
  _High betweenness centrality (0.137) - this node is a cross-community bridge._
- **Why does `RawEvent` connect `Adapter Framework` to `App Classification`, `Filesystem Adapter`, `Git State Tracking`, `Systemd Adapter`, `Podman Adapter`, `Udev Adapter`, `Bluetooth Adapter`, `Network RTNetlink`, `Network Adapter`, `Power Management`, `Hyprland Integration`, `UPower Integration`?**
  _High betweenness centrality (0.122) - this node is a cross-community bridge._
- **Why does `Config` connect `Adapter Configuration` to `Lua Runtime Core`, `App Classification`?**
  _High betweenness centrality (0.076) - this node is a cross-community bridge._
- **What connects `install.sh script`, `Git Adapter`, `Filesystem Adapter` to the rest of the system?**
  _44 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Lua Runtime Core` be split into smaller, more focused modules?**
  _Cohesion score 0.061946902654867256 - nodes in this community are weakly interconnected._
- **Should `Adapter Framework` be split into smaller, more focused modules?**
  _Cohesion score 0.0727036676403765 - nodes in this community are weakly interconnected._
- **Should `Adapter Configuration` be split into smaller, more focused modules?**
  _Cohesion score 0.0741745816372682 - nodes in this community are weakly interconnected._