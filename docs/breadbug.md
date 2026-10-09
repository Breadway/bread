# breadbug

breadbug lets a user report a crash, freeze or misbehavior of a bread app.
It is opt-in, previewable and scrubbed. Tracker: ECO-EP1 (spec ECO-66).

## What a user sees

1. An app crashes or freezes, or the user notices a problem.
2. They run `breadbug` (a keybind in BOS, the Bug reports page in
   bos-settings, or the "Review report" action on the crash notification).
3. They pick the app from the installed bread apps (recent problems first),
   pick crash, freeze or behavior, and may add notes.
4. breadbug shows exactly what would be reported: counts per section, what the
   scrubber replaced, a SHA-256 of the report, and the full text on request.
5. They choose **save to file**, **send**, or **cancel**. Nothing is sent
   unless they choose send, and sending must also be enabled in settings
   (`breadbug config upload on`). The default is off.

The bytes that were previewed are the bytes that are saved or sent. The report
is built once, serialized once and hashed; the upload path refuses to send if
the hash no longer matches.

## Where data comes from

breadd keeps a bounded on-disk store per app, so a dead or hung app never has
to be queried:

```
~/.local/state/bread/bugstore/apps/<app>/{log,events,markers}.jsonl  (+ .1)
```

Dirs are 0700, files 0600. Each file rotates at a size cap keeping one
previous generation; a global cap drops the oldest data first; there is a cap
on the number of apps. `breadbug` reads the same files directly, so it works
when breadd is down.

Feeders: apps push logs and markers over the `bug.log`, `bug.logs` and
`bug.marker` IPC methods (see Documentation.md); breadd records bus events
(names plus allowlisted fields) and its own tracing output.

Configuration lives in `~/.config/bread/breadbug.toml` (`upload_enabled`,
`excluded_apps`, `endpoint`, `watchdog_enabled`, `freeze_after_ms`, `[store]`
limits). Excluded apps store nothing
and appear in no report.

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

## Freeze detection

Apps send `bug.heartbeat` from their main loop. If heartbeats stop while the
process is still alive for longer than `freeze_after_ms` (default 15 s) and
four of the app's own intervals, breadd records a `freeze` marker and
publishes `bread.bug.freeze`; a later heartbeat records `recovered`. Detection
is paused for ten seconds after power or monitor events and after a stalled
daemon tick, so suspend/resume and display hotplug do not look like freezes.
An exited process is forgotten, not reported. Nothing is ever killed.

For a frozen app that is still running, `breadbug` can include a stack trace
(function names and offsets only) if the user agrees (`--live-stack` or the
prompt). The pid comes from the freeze marker and is re-checked to be a live
`bread*` process before anything attaches.

## Scrubbing

`bread-scrub` runs twice: on ingest into the store (so raw personal data never
reaches disk) and again over the whole report with a fresh scrubber.

* **Structured data is allowlisted.** Event payloads are dropped unless the
  allowlist names a field (power: `ac_connected`, `battery_percent`; network:
  `online`; device: `subsystem`, `vendor_id`, `product_id`; bluetooth:
  `subsystem`). A dropped payload leaves only the event name and plain key
  names.
* **Free text is pattern-redacted:** emails, IPv4/IPv6, MACs, UUIDs, long hex,
  known token formats, `key=value` secrets, URL credentials and query strings,
  home paths, user and host names, SSIDs, serials, window titles and
  high-entropy strings.
* **Crash data is frames only:** module, offset, build-id and an optional
  symbol. No memory, registers or environment.
* Build-ids are exempt from the hex rule; the server needs them to symbolicate.
* User notes are scrubbed too, and the prompt warns not to paste secrets.

Free-text scrubbing is best effort (it cannot know an arbitrary word is a
name). That is why the preview exists and why structured fields are allowlisted.

## Report schema (version 1)

| Field | Content |
|-------|---------|
| `schema` | `1`; bumped for shape changes the server must know about |
| `report_type` | `crash`, `freeze` or `behavior` |
| `created_unix` | seconds since the epoch |
| `app` | `{ id, version, build_id }` |
| `system` | OS id/version/build, kernel, arch, GPU drivers with PCI ids, compositor version, bakery track, breadd and API versions |
| `installed_apps` | `[{ id, version }]` |
| `runtime` | daemon running, uptime, adapter statuses, module counts |
| `markers` | up to 5 recent crash/freeze markers with frames |
| `logs`, `daemon_logs` | scrubbed app log lines; daemon warnings and errors |
| `events` | event names with allowlisted fields |
| `journal` | scrubbed `journalctl --user` lines for the app's processes |
| `live_stack` | frames of the live process, only if the user consented |
| `notes` | the user's own text, scrubbed, at most 8 KiB |
| `redactions` | count of replaced values per category |

The payload is capped at 1 MiB; the builder trims the journal, then the oldest
events and logs, to fit.

## Consent copy (single source for CLI, GUI, settings, help)

> A report contains your app's recent log lines, crash frames, event names and
> basic system facts such as OS, kernel, GPU driver and bread versions. Personal
> values such as your name, home folder, hostname, IP addresses, emails and
> tokens are replaced before anything is stored or shown. You can read the
> whole report before deciding. Nothing is sent unless you choose Send. You can
> save the report to a file instead.

## Server and threat model (summary; details in breadbug-server)

* The intake endpoint is public and unauthenticated: treat all input as
  hostile. Strict versioned schema validation, unknown fields rejected, hard
  size cap, rate limits per anonymous install-id, IPs never stored.
* The server re-scrubs everything (never trusts the client) and quarantines
  payloads that still look personal.
* Report text is never interpolated into shell, SQL or templates, is escaped
  wherever rendered, and is data (never instructions) to any LLM step.
* The deletion token is returned once and stored only as a hash. Raw reports
  are deleted after 90 days; cluster metadata is kept.
* Threats considered: oversize and decompression bombs, flooding, malformed
  JSON, log and markdown injection, XSS in the triage UI, prompt injection,
  scraping of other users' reports, and re-identification via free text.
