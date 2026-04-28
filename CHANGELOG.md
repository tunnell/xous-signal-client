# Changelog

All notable changes to `xous-signal-client`. Format follows [Keep a
Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Changed

- `main_ws::dispatch_envelope` now uses a single shared `Rc<pddb::Pddb>`
  across all five protocol stores instead of allocating a fresh
  `pddb::Pddb::new()` per store. Each `Pddb::new()` invokes
  `xns.request_connection_blocking` (an RPC); the consolidation drops
  per-envelope PDDB connection-request RPCs from 5 to 1, and `try_mount`
  calls from 5 to 1. New `*Store::new_shared(pddb: Rc<pddb::Pddb>, ...)`
  constructors expose the consolidation pattern; the existing
  `*Store::new(pddb: Pddb, ...)` constructors stay backward-compatible
  (they wrap in `Rc` internally). Closes #26.
- `tools/measure-renode.sh`: previously exited 2 (skip) when Renode
  refused to compile `LiteX_Timer_32.cs` due to a `long`/`ulong`
  mismatch against Renode 1.16.1. The cast itself is now fixed in
  `tunnell/xous-core` PR #18 (issue #13) so the script gets past
  peripheral compilation; an unrelated UART-capture limitation now
  causes it to skip instead. The skip path is preserved (Renode is
  optional infrastructure) and the new failure mode is documented.
  `tests/README.md` and `tests/renode/README.md` updated. UART
  wiring tracked as follow-up #34. Closes #13.
- `AccountAttributes` JSON payload (sent on link / registration; pre-required
  for the upcoming `PUT /v1/accounts/attributes` in #16) now omits three
  legacy fields: `signalingKey` (obsolete since libsignal Double Ratchet
  ~2017), `voice`, and `video` (modern Signal-Server carries voice/video
  capability under the `capabilities` sub-object, not at the top level).
  The remaining 10 top-level fields and the 5 capability sub-fields match
  modern Signal-Server's `AccountAttributes` entity. Three new unit tests
  pin the absence of the legacy fields and the exact set of top-level
  keys. Closes #17.
- WebSocket upgrade handshake now sends `X-Signal-Receive-Stories: false`
  (was `true`). xous-signal-client has no Story-rendering UI; asking the
  server for Story envelopes wasted bandwidth and decryption cycles on
  envelopes we silently dropped. Refactored the request-building out of
  `SignalWS::connect` into a pure `build_ws_upgrade_request` helper so
  the header set is unit-testable. Closes #18.
- `tools/scan-send.sh`, `tools/scan-receive.sh`, and `tools/demo-prep.sh`
  now share a single session-clearing helper
  (`xsc_clear_signal_cli_sessions` in `tools/test-helpers.sh`) called
  before the priming send. Forces signal-cli to issue a PreKey-bundle
  (envelope type 3) instead of reusing a stale session and sending a
  SignalMessage (envelope type 1) that the rolled-back PDDB cannot
  decrypt. Fixes the B2-sibling priming flake that caused intermittent
  scan-send / scan-receive failures (issue #9). `tools/run-all-tests.sh`
  now reaches Family 2 reliably without manual `demo-prep.sh` runs.

### Added

- Three unit tests for `PddbKyberPreKeyStore::mark_kyber_pre_key_used`
  covering libsignal's last-resort semantics: first-call writes the
  dedup marker and returns `Ok`, second call with the same
  `(kyber_id, ec_prekey_id, base_key)` tuple is rejected with
  `InvalidKyberPreKeyId`, and calls with different `ec_prekey_id` or
  different `base_key` produce independent dedup entries. The
  implementation was already correct (commit `6c0935b`,
  2026-04-24) — these tests close the test-coverage gap flagged as
  issue #11. Marked `#[ignore]` like the rest of the PDDB store
  tests (require Xous IPC server; run inside the emulator).
- `XSC_DEMO_PEER_UUID` (and optional `XSC_DEMO_PEER_DEVICE_ID`) env-var
  seam: pre-seeds the V1 default outgoing recipient at startup so a
  hosted-mode session can send the first message without first having
  received one. Validates UUID format and device-id range; falls back
  to current behavior on unset/invalid input or if a recipient is
  already persisted in PDDB. Eight new unit tests in
  `manager::outgoing::tests::parse_demo_peer_*`.
- Right-aligned local-author bubbles. `main.rs` calls
  `chat.set_author_flags("me", AuthorFlag::Right)` after `dialogue_set`
  so outgoing local-echo bubbles render on the conventional sender
  side. Depends on the `Chat::set_author_flags` API added to chat-lib
  on the project's pinned `feat/05-curve25519-dalek-4.1.3` branch
  (tracked in `xous-signal-client-notes/techContext.md` patch table).
- `tools/demo-prep.sh`: recording-day setup script. Restores the PDDB
  snapshot, looks up the emulator's UUID via signal-cli's `recipient`
  table by phone number, deletes any stale `session` rows for that
  UUID (B2-sibling priming-flake mitigation per
  `bug-arcs/b005-signal-cli-libsignal-decrypt.md`), and runs
  `scan-receive.sh` once to warm up a clean session.
- `AGENTS.md`, `ARCHITECTURE.md`, `CHANGELOG.md`: project documentation
  derived from a consolidation pass.
- `docs/decisions/`: 9 MADR-format ADRs covering hand-rolled libsignal-
  protocol orchestration, canonical proto field tags, multi-device fan-
  out, sync transcripts, testing methodology, KNOWN_FAIL convention,
  diagnostic instrumentation, PDDB stores schema, and worker-thread
  WebSocket pattern.
- Maintenance contract section in `AGENTS.md` codifying the working
  agreement that documentation is maintained as part of code changes.
- `.gitignore` patterns for PDDB snapshot files (sensitive credential
  material).
- Additional unit tests for `trust_mode`, `link_state`,
  `service_environment`, `main_ws::strip_signal_padding`, timeout
  helpers, and the AES-CTR wrapper (ported from a parallel testing-
  infrastructure branch).

### Open known issues

- **B2** — signal-cli libsignal decrypt failure on post-409-retry
  CIPHERTEXT. KNOWN_FAIL handling stays in `tools/scan-send.sh` and
  `tools/run-all-tests.sh`. As of the 2026-04-28 dedicated investigation,
  the documented send-direction failure is **not currently
  reproducible** (5/5 consecutive scan-send PASSes exercising the
  409 retry path). The PR #4 chain-counter-advance hypothesis is
  contradicted by the repeated successful decrypts. KNOWN_FAIL stays
  in place because the same libsignal failure-mode string surfaced in
  the *receive* direction during the investigation, triggered by
  PDDB-snapshot rollback while signal-cli's session state moves
  forward across runs. See bug arc and `tests/known-issues.md`.

## [0.0.4] - 2026-04-27 — commit `5117925` (PR #4)

### Added

- `tools/scan-send.sh` now runs `signal-cli receive` after leg-1 PASS,
  enforcing the three-legged stool of verification.
- `KNOWN_FAIL` test status convention via exit code 87. See ADR 0006.
- `tests/known-issues.md` — anchored doc for documented-but-unfixed
  failures.

### Changed

- `tools/run-all-tests.sh` maps exit 87 → KNOWN_FAIL in summary;
  orchestrator exits 0 (KNOWN_FAIL is non-blocking).
- Summary column widened from `%-8s` to `%-12s` to accommodate
  `KNOWN_FAIL` without truncation.

## [0.0.3] - 2026-04-27 — commit `d67a55b` (PR #3)

### Added

- `tools/scan-receive.sh` (232 lines): hosted-mode receive driver.
- `tools/test-helpers.sh::xsc_verify_linked_device` — topology
  pre-check via `signal-cli listDevices`.
- `XSCDEBUG_RECV=1` env-var-gated `[recv-debug]` log line in
  `main_ws.rs::deliver_data_message` and `::deliver_sync_message`.
- Priming pattern: scan scripts send a fresh PreKey envelope before
  emulator boot to refresh ratchet state from PDDB snapshot.

### Changed

- `tools/run-all-tests.sh` orchestrator now reports four families
  (rust, send, recv, footprint) instead of three.
- Both scan scripts now refuse to proceed if expected linked
  secondary (`signal-cli-test`) is absent on the verifier account.

## [0.0.2] - 2026-04-27 — commit `7f9b644` (PR #2)

### Added

- `tests/README.md`: testing methodology + six principles
  extracted from the Phase A bug arc.
- `tools/scan-send.sh`: hosted-mode E2E test driver.
- `tools/decode-wire.sh`: protobuf wire-byte decoder with canonical
  field-tag conformance checks.
- `tools/measure-size.sh`: thin wrapper around
  `.github/scripts/check_size_budget.py`. Treats TOTAL-only breach as
  PASS-WITH-NOTE per project policy.
- `tools/measure-renode.sh`: Renode boot smoke. Detects known
  `LiteX_Timer_32.cs(23,62)` peripheral incompatibility and reports
  SKIPPED.
- `tools/run-all-tests.sh`: three-family orchestrator.
- `tools/test-env.example`; `tools/.env` gitignored.
- README Testing section.

### Changed

- No production source code changed in this PR.

## [0.0.1] - 2026-04-27 — commit `7786455` (PR #1)

### Added — the big end-to-end-success PR

- **Multi-device send fan-out** (commit `ba783ee`).
  - `DeviceSessionEnum` trait abstraction.
  - `PddbSessionStore::device_ids_for(uuid)` enumeration.
  - `submit_with_retry_generic` re-encrypts per session-device per
    iteration. Reference pattern adapted from
    `whisperfish/libsignal-service-rs/sender.rs` (AGPL-3.0).
- **Sync transcripts** (`SyncMessage::Sent`) for own-account secondary
  devices (commit `0429924`).
- **DataMessage.timestamp at canonical proto field tag 7** (commit
  `da08f2e`). Was at tag 5 (`expireTimer`) due to manual proto
  definition; B1 from the V5 audit. Symmetric receive-side fix at the
  same tag.
- **PERMISSIVE base64 decoder for prekey-bundle responses** (commit
  `089be8e`). Server returns prekey-bundle base64 unpadded; padded
  `STANDARD` decoder rejected.
- **`StatefulMockHttp` test infrastructure** that simulates Signal-
  Server's actual 409 behaviour (registered devices vs. body's
  `messages[]`).
- All commit trailers normalized to `Generated with an AI agent.` +
  DCO `Signed-off-by:`. README Acknowledgement section.

### Removed

- `docs/research/` (internal design notes carryover from initial
  fork): UI conversation list, memory budget, concurrency
  architecture. Not appropriate to ship publicly. Preserved
  internally.
