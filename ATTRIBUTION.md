# Attribution

## Project lineage

- **chat-lib substrate:** [betrusted-io/xous-core/libs/chat](https://github.com/betrusted-io/xous-core/tree/main/libs/chat)
  by @nworbnhoj. Provides the chat UI framework, PDDB-backed conversation
  storage, and IPC opcode pattern that this project builds on.

- **mtxchat (Matrix client):** [betrusted-io/xous-core/apps/mtxchat](https://github.com/betrusted-io/xous-core/tree/main/apps/mtxchat).
  The structural template that sigchat (and this project) follow for
  chat-lib-based apps.

- **sigchat:** [betrusted-io/sigchat](https://github.com/betrusted-io/sigchat).
  The Signal-protocol scaffolding this project started from. The Phase 2a
  (libsignal-based outgoing message encryption) and Phase 2b (REST submission
  with 409/410 retry) work in this repo was originally implemented in a fork
  of sigchat at tunnell/sigchat.

## Protocol

- **Signal protocol:** [signalapp/libsignal](https://github.com/signalapp/libsignal),
  AGPL-3.0. Used as a Cargo dependency.

- **Renode emulation tooling:** Robot Framework primitives borrowed from
  [antmicro/renode-xous-precursor](https://github.com/antmicro/renode-xous-precursor),
  Apache-2.0.

## File-level attribution

Files adapted from upstream sources carry an attribution header indicating
their origin. Files originally written for tunnell/sigchat and ported here
are noted as such.

## Trademark

"Signal" is a registered trademark of Signal Messenger LLC. This project is
unaffiliated with the Signal Foundation. The name "xous-signal-client"
describes a client that speaks the Signal protocol; it does not claim to be
Signal.
