# breadbug

breadbug lets a user prepare a report about a bread app crash, freeze or
misbehavior. The local capture, scrub, preview, file export and opt-in upload
client are implemented. The server-side intake service is not part of this
repo. Tracker: ECO-EP1 (design spec ECO-66).

## What a user sees

1. An app crashes or freezes, or the user notices a problem.
2. They run `breadbug` (a keybind in BOS, the Bug reports page in
   bos-settings, or the "Review report" action on the crash notification).
3. They pick an app from the installed bread apps (recent problems first),
   pick crash, freeze or behavior, and may add notes.
4. breadbug shows exactly what would be reported: counts per section, what the
   scrubber replaced, a SHA-256 of the report, and the full text on request.
5. They choose **save to file**, **send**, or **cancel**. Sending requires both
   `upload_enabled = true` and a separate explicit Send action. The setting is
   false by default. When sending is off, nothing touches the network.

The report is built once, serialized once and hashed; the preview, file export
and upload all use those bytes. The client re-verifies the hash immediately
before sending. Local `--save` writes mode 0600.

## Where data comes from

breadd keeps a bounded on-disk store per app, so a dead or hung app never has
to be queried:

```
~/.local/state/bread/bugstore/apps/<app>/{log,events,markers}.jsonl  (+ .1)
```

Current defaults in `bread-bug/src/config.rs`: each file generation is capped
at 256 KiB, total store at 32 MiB, 64 app directories and 4096 bytes per stored
line. At rotation only one previous generation is kept; oldest data is dropped
at the global cap. Directories are 0700 and files 0600. These are configurable
through `[store]` in `~/.config/bread/breadbug.toml`. `breadbug` reads the files
directly and can build a report when breadd is down.

Feeders send logs and markers over IPC; breadd records event names plus the
field allowlist below and its own tracing output. The app exclusion list is
`excluded_apps`; excluded apps store nothing and are omitted from reports.
The code supports exclusions but does not ship a hard-coded sensitive-app
policy. The design requires sensitive apps such as breadclip (clipboard
contents) and breadsearch (queries) to be excluded by default, with a clear,
user-controlled way to retain an exclusion. That default policy is not yet
implemented. No clipboard contents or search queries are allowed report fields.

Configuration lives in `~/.config/bread/breadbug.toml` (`upload_enabled`,
`excluded_apps`, `endpoint`, `watchdog_enabled`, `freeze_after_ms`, `[store]`
limits).

## Sending

Sending needs `upload_enabled = true` in `breadbug.toml` (or `breadbug config
upload on`); with it off, nothing in breadbug touches the network, including
the queue, `history --refresh` and `delete`.

* The endpoint must be https (plain http only for loopback) and redirects are
  never followed.
* The previewed bytes are sent unchanged with `X-Breadbug-Sha256`; the client
  re-verifies the hash right before sending, and the server checks it too.
* `X-Breadbug-Install` is a random 128-bit id kept in
  `~/.local/state/bread/breadbug/install.json`, replaced every 30 days. There
  is no account and no stable device identifier.
* If the server is unreachable (network error, 429, 5xx) the exact bytes go to
  an offline queue (at most 20) and are retried with exponential backoff up to
  6 hours via `breadbug queue flush`. A report the server rejects is dropped.
* A successful send returns a receipt and a deletion token, kept in
  `history.jsonl` (0600). `breadbug delete <receipt>` removes it from the
  server. An identical resend returns the original receipt.

## Status and fix notifications

`breadbug status` asks the server how the reports you sent are doing
(`received`, `duplicate`, `investigating`, `fixed` in a version, `closed`),
updates your history, and tells you when one is fixed in a version newer than
the one you have installed. With `notify_fixed = true` in `breadbug.toml`
(off by default, a bos-settings toggle) and `--notify` it also shows one
desktop notification per fix. It only ever polls receipts you hold, never
sends anything, and does nothing at all while sending is off. A daily user
timer (`packaging/systemd/breadbug-status.timer`) runs it quietly; there is no
push channel and no email.

## Freeze detection

Apps send `bug.heartbeat` from their main loop. If heartbeats stop while the
process is still alive for longer than `freeze_after_ms` (default 15 s) and
four of the app's own intervals, breadd records a `freeze` marker and
publishes `bread.bug.freeze`; a later heartbeat records `recovered`. Detection
is paused for ten seconds after power or monitor events and after a stalled
daemon tick, so suspend/resume and display hotplug do not look like freezes.
An exited process is forgotten, not reported. Nothing is ever killed.

For a frozen app that is still running, `breadbug` can include a stack trace
(module names, module-relative offsets and build ids only) if the user agrees
(`--live-stack` or the prompt, plus a separate confirmation in the GUI). The
pid and process identity come from the freeze marker and are re-checked
immediately before attaching. Frames never include memory, registers,
environment variables or a core dump.

## Scrubbing

`bread-scrub` runs on ingest into the store and again over a report. Structured
event payloads use an allowlist, not a denylist. For v1, the allowed event
fields and JSON types are:

| Event pattern | Allowed field | Type / bound |
|---------------|---------------|--------------|
| `bread.power.**` | `ac_connected` | boolean |
| `bread.power.**` | `battery_percent` | integer, 0-100 |
| `bread.network.*` | `online` | boolean |
| `bread.device.*` | `subsystem` | string, scrubbed and limited by the configured 4096-byte stored-line cap |
| `bread.device.*` | `vendor_id`, `product_id` | identifier strings, scrubbed and limited by the configured 4096-byte stored-line cap |
| `bread.bluetooth.**` | `subsystem` | string, scrubbed and limited by the configured 4096-byte stored-line cap |

Only the named fields from matching event names survive. Unknown event names,
fields and payload shapes are dropped; event name and timestamp remain. The
current generic event-field implementation does not enforce the table's
semantic JSON types/ranges. Enforcing these types and bounds at ingestion and
server validation is a design requirement, not an implemented guarantee.

Other v1 fields are explicitly limited to these shapes:

* `system`: optional OS id/version/build, kernel, compositor, bakery track,
  breadd/API versions; architecture string; GPU driver plus PCI vendor/device
  identifiers. No machine name, user, network address, serial or arbitrary
  hardware inventory. Values are scrubbed strings; no field-specific byte caps
  beyond the 1 MiB report cap are enforced today.
* `installed_apps`: array of `{id, version}` for recognized installed bread
  apps in bakery state. Does not enumerate unrelated software. The array has no
  separate count cap; the report cap is 1 MiB.
* `runtime`: `daemon_running` boolean; optional `uptime_s` unsigned integer;
  adapter-name to status-string map; optional unsigned `modules_total` and
  `modules_with_errors`. This is a coarse health summary, not module config or
  arbitrary process state. Map counts and string sizes have no separate cap
  today.
* `markers`: at most the newest 5 markers per report. Each includes timestamp,
  kind (`panic`, `crash`, `freeze`, `recovered`; local-only `stopped` markers
  close a retired freeze and are not problems), optional app version, build-id,
  message, location, signal and at most 128 frames per stored marker. The
  freeze marker's pid and process identity (executable, boot id and kernel
  start ticks) are local bookkeeping used only to verify a consented live
  capture; they are removed before report serialization. A frame
  contains optional module and symbol strings, unsigned module-relative offset,
  and build-id. Message/location/module/symbol are scrubbed strings; the 4096
  byte store-line limit applies on ingest. Report-level trimming can still fail
  if the remaining non-trimmable data exceeds 1 MiB.
* `logs` and `daemon_logs`: arrays of `{t, level, msg}` scrubbed records. The
  builder requests at most 300 lines per app and selects only WARN/ERROR daemon
  logs. Per-file/store limits above apply; report builder trims old app and
  daemon logs after journal/events as necessary.
* `journal`: strings from at most 150 `journalctl --user` lines for the selected
  app process names; each line is cut at 2048 bytes. Journal data is optional
  in the builder and is trimmed first to fit the report cap.
* `live_stack`: array of frame records with the same frame shape as markers,
  supplied only after a distinct, informed, per-report user consent. It may
  include only module, module-relative offset, build-id and optional symbol.
  It never serializes process memory, registers, environment variables or a
  core dump.
* `notes`: optional scrubbed string, truncated at 8 KiB UTF-8 bytes.
* `redactions`: map from scrub category string to unsigned count.

Free-text scrubbing is best effort: it cannot know an arbitrary word is a
name. Build-ids are exempt from long-hex redaction for symbolication. The
preview exists so users can inspect the exact payload.

## Consent copy (intended user-facing copy)

The text is `bread_bug::consent::CONSENT_TEXT`. It is shown once, before the
first interactive `breadbug` session (Enter acknowledges and records it; `q`
stops without recording, so it is shown again next time). The GUI,
bos-settings and breadhelp reuse the same constant. In short:

> A report contains your app's recent log lines, crash frames, event names and
> basic system facts such as OS, kernel, GPU driver and bread versions.
> Personal values such as your name, home folder, hostname, IP addresses,
> emails and tokens are replaced before anything is stored or shown. You can
> read the whole report before deciding. Nothing is sent unless you choose
> Send, and sending is off by default. You can save the report to a file
> instead.

The configured default endpoint is `https://bugs.breadway.dev`. Nothing is
sent unless upload is enabled and the user chooses Send for that report, and
`breadbug status` only polls receipts the user holds. The intake service and
its retention controls are not part of this repo.

## Server trust boundaries and threat model (requirements, not current controls)

The public upload service is a separate trust boundary. Before launch, route
public traffic through Aegis Caddy to an isolated intake service on Hestia; do
not expose the application listener directly. Validate TLS/host and request
size at the edge and again in the service. Use strict versioned-schema
validation, reject unknown fields, cap decompressed and serialized sizes, and
never trust client-side scrubbing. Treat report contents as hostile: parameterize
database access, escape rendered text, prevent log/markdown injection, and
pass text to any future LLM as untrusted data, never instructions. Do not send
data to third-party model APIs.

The intake service must have no shared credentials with Forgejo, bread services
or other containers. Store raw reports separately from derived cluster
metadata. Symbol files must live in a dedicated, read-only-to-symbolicator
store, keyed by build-id, isolated from intake and public web paths. Only the
symbolication worker may read symbols; intake, triage UI, unrelated services
and public users must not have filesystem or API read access. CI publication,
retention and access auditing for symbols must be defined before launch.

For abuse controls, use a rotating, random anonymous install-id that contains
no account or hardware identity. Bind it to a server-issued token or keyed
server-side representation; rotate it on a documented schedule and avoid
storing raw IP addresses. Apply per-install and per-IP short-window rate limits
without retaining IPs in report records, global abuse/flood limits, request and
concurrency caps, and a quarantine path for malformed/suspicious reports. A
rotating identifier is not authentication and can be reset or spoofed; it must
not be treated as proof of a unique person or device. These are launch
requirements. This repo implements the client-side anonymous install-id and
client receipt/deletion-token storage, not the network intake, edge, rate
limit, isolated-store or symbol-store controls.

Proposed retention is raw reports for 90 days with cluster metadata retained.
Deletion by receipt must be designed to cover raw report copies and backups;
state explicitly what cannot be removed, including derived cluster metadata.
No server-side deletion endpoint, retention enforcement, server validation,
server-side rescrubbing or triage UI is implemented in this repo.

Threats to design for include oversize/decompression bombs, flooding, malformed
JSON, log and markdown injection, XSS in triage, prompt injection, cross-user
report access and re-identification from free text. A human should promote
clusters to breadboard issues; automated issue creation is out of scope until
clustering is proven safe.
