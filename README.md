# xous-signal-client

Unofficial Signal client for [Xous](https://github.com/betrusted-io/xous-core)
on [Precursor](https://www.crowdsupply.com/sutajio-kosagi/precursor).

**Prototype.** Not for production use.

## Status

Pre-alpha. Linking and 1:1 message send/receive are partially implemented.
Binary is over its target size budget. UI flow is incomplete.

This is not an official Signal product. The Signal name is a trademark of
Signal Messenger LLC. This project is unaffiliated with the Signal Foundation.

## Build

Sibling-checkout pattern:

    git clone https://github.com/betrusted-io/xous-core.git
    git clone https://github.com/tunnell/xous-signal-client.git

For hosted mode:

    cd xous-signal-client
    cargo build --release --features hosted

For Precursor / Renode:

    cargo build --release --target=riscv32imac-unknown-xous-elf --features precursor

Then back in `xous-core`:

    cargo xtask run xous-signal-client:../xous-signal-client/target/release/xous-signal-client

(or `app-image` for hardware/Renode).

## Provenance

Built on [chat-lib](https://github.com/betrusted-io/xous-core/tree/main/libs/chat),
the Xous chat UI framework that also powers
[mtxchat](https://github.com/betrusted-io/xous-core/tree/main/apps/mtxchat) (Matrix).
Originally forked from [sigchat](https://github.com/betrusted-io/sigchat),
which itself was templated from mtxchat. See ATTRIBUTION.md for full provenance.

The libsignal protocol layer is from
[signalapp/libsignal](https://github.com/signalapp/libsignal).

## License

Apache-2.0 OR AGPL-3.0 (you choose). The libsignal dependency is AGPL-3.0;
if your usage links libsignal, choose AGPL.

## Acknowledgement

This project was developed with help from AI coding assistants.
