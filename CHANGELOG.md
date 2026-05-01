# Changelog

All notable changes to `xous-signal-client`. Format follows [Keep a
Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- One-time EC prekey replenishment via `PUT /v2/keys` (issue #15).
  After every `Manager::start_receive`, a worker thread queries
  `GET /v2/keys?identity=aci` and, if the server-reported count is
  below 10, generates a batch of 100 fresh X25519 one-time prekeys,
  persists them to `sigchat.prekey/<id>`, and uploads them. Constants
  match `libsignal-service-rs` (`PRE_KEY_MINIMUM=10`,
  `PRE_KEY_BATCH_SIZE=100`). A new persistent counter at
  `sigchat.account/aci.next_prekey_id` allocates IDs sequentially
  with a small random initial seed, wrapping at `MEDIUM_MAX`.
  Replenishment failures are non-fatal and log-only — the receive
  worker is not blocked. Closes #15. Architectural rationale,
  decision-points, and the deferred-follow-up list are in ADR 0013.

  This also supplies the **initial fill** of one-time prekeys, which
  the link flow has never done — before this PR, every peer's first
  message to our account fell back to the Kyber-last-resort-only path
  with degraded forward-secrecy.

  **Test-workflow heads-up:** landing this PR causes
  `scan-receive.sh` to fail with `InvalidPreKeyId` against a pre-PR
  PDDB snapshot, because the server now advertises prekey IDs whose
  private halves are not in the snapshot. In production this cannot
  happen (PDDB is never rolled back). See `tests/known-issues.md`
  entry "Stale prekey snapshot divergence after #15 lands" for
  mitigation options.
- Conversation-list UI (Phase D, issue #24). Pressing F1 inside the
  chat surfaces a `modals` radio-button picker listing every peer we
  have a conversation with, sorted by last-message timestamp
  descending, with a leading `*` marker on rows with unread messages
  and a one-line snippet of the most recent message. Selecting a peer
  swaps chat-lib's active dialogue to that peer's conversation, clears
  that peer's unread counter, and persists the focused peer so
  subsequent outbound posts from `SigChat::post` are routed there.
  Pressing F1 again from inside any conversation re-opens the picker.
  New `manager::peers` module owns per-peer summary metadata under a
  new `sigchat.peers` PDDB dict. Inbound `DataMessage`s now route to
  per-peer dialogues (`sigchat.dialogue/<uuid>`) instead of the single
  hardcoded `default` dialogue; sync-transcripts (own outbound from
  another linked device) route to the destination peer's dialogue.
  Architectural rationale and the deferred-features list are in ADR
  0012.
- `manager::rest::put_accounts_attributes`: post-link `PUT /v1/accounts/attributes`
  refresh. Called from `Account::link` after the link succeeds. The link body
  already carries an `accountAttributes` sub-object that updates the device
  record; this separate PUT updates the canonical per-account record so
  Signal-Server's per-device and per-account views agree (matching reference
  clients: signal-cli, libsignal-service-rs, Signal-Android). Failure is
  non-fatal — link succeeded, message receive path works, attributes can be
  retried on a future startup. Identifier format is `<aci>.<deviceId>`.
  `AccountAttributes` and `Capabilities` now derive `Clone` so the link
  flow can pass one copy to the link body and keep another for the
  follow-up PUT. Two new unit tests pin the new identifier format and
  the Clone serialization equivalence. Closes #16.

### Changed

- Renamed the pinned `tunnell/xous-core` branch from
  `feat/05-curve25519-dalek-4.1.3` to `dev-for-xous-signal-client`.
  The old name encoded its leading patch (curve25519-dalek 4.1.2 →
  4.1.3 bump); the branch now carries multiple unrelated patches
  (Renode `LiteX_Timer_32` cast, TLS refactor + revert, chat-lib
  `set_author_flags`) and the purpose-named ref is more
  discoverable. Updated AGENTS.md, TESTING-PLAN.md, tests/README.md,
  the four `tools/*.sh` scan scripts, and the `XOUS_CORE_BRANCH`
  env in both size-CI workflows. ADR 0012 retains the old name as
  historical record (MADR convention: ADRs are immutable). The
  GitHub branch rename was atomic; no PRs were affected (none were
  open against the old ref).
- ADR 0011 (`docs/decisions/0011-affirm-hand-rolled-with-stop-loss-criteria.md`)
  reaffirms the hand-rolled libsignal-protocol orchestration choice and
  replaces ADR 0001's "open architectural alternative" caveat with concrete
  stop-loss criteria for re-opening the libsignal-service-rs migration
  question. ADR 0001's status line now points at 0011. Closes #23.
- B2 (issue #8) closed: signal-cli libsignal `InvalidMessageException`
  on the emulator's post-409-retry CIPHERTEXT is no longer reproducible
  after the receive-direction priming-flake fix in PR #30 (issue #9).
  Three consecutive `scan-send.sh` runs all PASSed leg-1 + leg-2.
  Removed the `KNOWN_FAIL` exit-87 mapping from `tools/scan-send.sh`
  and `tools/run-all-tests.sh`; moved B2 to `tests/known-issues.md`'s
  "Resolved" section. `scan-send.sh` retains a defensive recognizer
  that emits a "B2 regression?" diagnostic if the pattern ever
  re-occurs (exits 1, not 87).
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
  on the project's pinned `dev-for-xous-signal-client` branch
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
- `docs/decisions/0010-outbound-datamessage-omits-profilekey.md`:
  ADR documenting the decision to leave `profileKey` absent from
  outbound `DataMessage`. Not a delivery fix (V6/V7 demonstrated
  delivery works without it); first-contact display-name UX is a
  separate future enhancement. Closes #19.
- Maintenance contract section in `AGENTS.md` codifying the working
  agreement that documentation is maintained as part of code changes.
- `.gitignore` patterns for PDDB snapshot files (sensitive credential
  material).
- Additional unit tests for `trust_mode`, `link_state`,
  `service_environment`, `main_ws::strip_signal_padding`, timeout
  helpers, and the AES-CTR wrapper (ported from a parallel testing-
  infrastructure branch).

### Open known issues

*(none currently — B2 closed 2026-04-28; see `tests/known-issues.md`
"Resolved" for historical entries)*

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
