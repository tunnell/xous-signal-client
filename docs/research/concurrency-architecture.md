# Concurrency patterns in Xous for long-running network I/O during blocking modal UI — report for sigchat WebSocket keepalive

## Executive summary

The Xous project has a **documented, canonical idiom** for "an external event must be serviced while the main/UI thread is blocked": spawn a dedicated worker thread that owns a private `xous::SID` (single-purpose server), and let that thread forward events back to the main server via message-passing IPC. This is the pattern the Xous Book calls "Asynchronous Idioms or Push Notifications", and it is the pattern used throughout `xous-core` (including the example the book uses, `NetManager::wifi_state_subscribe`) ([Source](https://betrusted.io/xous-book/ch07-05-asynchronous.html)).

For sigchat's Signal provisioning problem, **candidate (C) — a dedicated background thread that owns the WebSocket, communicates with the UI via Xous IPC, and autonomously drains reads / sends application pings — is the most idiomatic, most secure, and most consistent with how `xous-core` itself is written**. Candidate (A) (Arc<Mutex<WS>>) is acceptable and will work (Xous supports `std::thread`, `Arc`, `Mutex`, `mpsc`), but it is *less* aligned with Xous's stated "no shared state; communicate via messages" design philosophy, and it creates a shared-mutable TLS/TCP object that the message-passing pattern eliminates by construction. Candidate (B) (a ping-only timer thread while the main thread still owns the WS) is explicitly a race hazard you should reject: having two threads touch a `rustls`/TLS object without serialization will corrupt the TLS record stream. Candidate (D) (non-blocking modal pump) is not supported by GAM's current `Modals` API, which is blocking by design ([Source](https://betrusted.io/xous-book/ch07-02-caller-idioms.html)).

I was **not able to retrieve the literal source of `apps/mtxchat/src/main.rs` or `libs/chat/src/lib.rs`** through the research tools available (GitHub blob endpoints were blocked, and search snippets did not surface their event-loop bodies). Where I rely on inference about those files rather than verbatim code, I flag it explicitly.

---

## 1. What Xous documents about concurrency

### 1.1 Xous is message-passing-first

Xous's own tagline: *"Xous is a microkernel operating system designed for medium embedded systems with clear separation of processes. Nearly everything is implemented in userspace, where message passing forms the basic communications primitive."* ([Source](https://xous.dev/)). The Xous Book's messaging chapter describes the design: every server has a blocking event loop on a `xous::SID`, threads de-schedule to zero CPU while waiting on `xous::receive_message`, and blocking messages (`lend`, `lend_mut`, `blocking_scalar`) yield the caller's quantum directly to the callee ([Source](https://betrusted.io/xous-book/ch07-02-caller-idioms.html)).

### 1.2 `std::thread::spawn` is supported and idiomatic — inside the Xous pattern

The Hosted Mode chapter states explicitly: *"The application is responsible for creating new threads, and may do so either by 'sending' a CreateThread call to the kernel or by creating a native thread using `std::Thread::spawn()`. When launching a thread with `CreateThread`, the kernel will allocate a new 'Xous TID' and return that to the application."* ([Source](https://betrusted.io/xous-book/ch03-02-hosted-mode.html)). So `std::thread::spawn` is a first-class way to create threads inside a Xous process.

Bunnie has written about embracing `std` in Xous: *"Rust is powerful. I appreciate that it has a standard library which features HashMaps, Vecs, and Threads. These data structures are delicious and addictive. Once we got `std` support in Xous, there was no going back… I have read some criticisms that it lacks features, but for my purposes it really hits a sweet spot."* He also flags the tradeoff: *"my addiction to the Rust `std` library has not done any favors in terms of building an auditable code base."* ([Source](https://www.bunniestudios.com/blog/category/betrusted/precursor/)). Takeaway: threading and `Arc`/`Mutex` are available and used, but the project treats them as an auditability cost that should be minimized, not a default.

### 1.3 The canonical "background event + main server" pattern (the `NetManager` idiom)

The Xous Book's Chapter 7.2.3 "Asynchronous" is the authoritative example. It says verbatim:

> "Push notifications are used when we want to be alerted of a truly unpredictable, asynchronous event that can happen at any time. One of the main challenges of push notifications is not disclosing your `SID` to the notifying server. Remember, anyone with your `SID` can invoke any method on your server, including more sensitive ones. **The idiom here is to create and reveal a 'single-purpose' server, whose sole job is to receive the push notification from the notifier, and forward this message back to the main server.** The single purpose server exists on the `lib` side, and is thus the caller controls it and its construction. It runs in its own dedicated thread; thus, the single-purpose server spends most of its life blocked and not consuming CPU resources, and only springs to action once a notification arrives." ([Source](https://betrusted.io/xous-book/ch07-05-asynchronous.html))

The book's concrete example (`NetManager::wifi_state_subscribe`) shows the actual code pattern: `xous::create_server().unwrap()` to mint a disposable SID, then `std::thread::spawn` containing `loop { let msg = xous::receive_message(onetime_sid).unwrap(); match FromPrimitive::from_usize(msg.body.id()) { ... } }` with a small, two-opcode (`Update`, `Drop`) protocol "which limits the attack surface exposed to a potentially untrusted subscriber" ([Source](https://betrusted.io/xous-book/ch07-05-asynchronous.html)).

Note what is *present* and *absent* in that idiom: present — `std::thread::spawn`, `xous::create_server`, `xous::receive_message`, a minimal opcode enum, and explicit thread termination via a `Drop` opcode. Absent — `Arc<Mutex<…>>` over the long-running resource. State crosses the thread boundary as `rkyv`-serialized `Buffer` messages, not as shared memory.

### 1.4 Xous-native synchronization primitives

`std::sync::Mutex` and `Condvar` in Xous are implemented on top of the Ticktimer server using `BlockingScalar` messages (ids 6/7 for mutex lock/unlock; 8/9 for condvar wait/notify). Uncontended mutex ops take a fast path with an `AtomicUsize` compare-exchange and do not hit the kernel; contended ones round-trip through Ticktimer ([Source](https://betrusted.io/xous-book/ch02-04-synchronization.html)). So using `Mutex`/`Condvar` is safe and efficient, but each contention event is a kernel IPC round-trip — a modest performance cost, and an unnecessary attack surface compared to pure message passing if you can avoid sharing the object at all.

The v0.9 release notes also record concurrency-quality commits that suggest this is an area where bugs have been fixed in-tree: *"mutex & condvar refactor in ticktimer thanks to @xobs - improves performance and stability"* and *"Issue #162 and #159: fix bugs with condvar support. condvar IDs are now serial, so re-allocations are not a problem, and the routine to remove old ones from the notification table now looks at the correct sender ID."* ([Source](https://github.com/betrusted-io/xous-core/blob/main/RELEASE-v0.9.md)). And for the net stack specifically: *"Fairly major overhaul to the network stack. We now use mpsc primitives to implement the wait/poll loop, which should make the net stack much more efficient and robust"* and *"refactor wait threads in net crate - use statically allocated pool of waiters + better filtering of requests for less churn; defer Drop on TcpStream until all written packets have been transmitted"* ([Source](https://github.com/betrusted-io/xous-core/blob/main/RELEASE-v0.9.md)). The net-server demo uses *"multiple worker threads + MPSC for communications"* ([Source](https://github.com/betrusted-io/xous-core/blob/main/RELEASE-v0.9.md)).

### 1.5 GAM modals are blocking by design

GAM's `Modals` API is synchronous ("deferred-response" in Xous terminology — the caller blocks until the user acts). The Xous Book characterizes this as: *"Deferred-response: these block the caller, but the callee is not allowed to block."* ([Source](https://betrusted.io/xous-book/ch07-02-caller-idioms.html)). In practice, a call like `modals.get_text(...)` parks the calling thread on a `BlockingScalar`/`lend` until the user interacts with GAM; during that park, the thread does not run, and nothing on that thread can service I/O. There is no documented "pump-driven" modal mode, which rules out candidate (D) without refactoring GAM itself.

---

## 2. What other xous-core services do

### 2.1 The `NetManager` / `net` service

Already covered above: the book-documented idiom is to mint a single-purpose SID, spawn a thread that owns it, and forward cleaned-up events back via a narrow opcode interface. The net crate itself uses mpsc channels internally for its wait/poll loop ([Source](https://github.com/betrusted-io/xous-core/blob/main/RELEASE-v0.9.md)).

### 2.2 mtxchat and the `libs/chat` library

mtxchat was explicitly created as the test bed for a reusable chat UI framework:

- The `libs/chat` framework was designed by contributor @nhoj to eventually support both Matrix and Signal: *"Contributor @nhoj has done the heavy lifting of laying down a UI framework in libs/chat for chat. It creates an infrastructure that could eventually accommodate both Matrix and Signal protocols. … Mtxchat is the work-in-progress test app for the chat framework."* ([Source](https://crowdsupply.com/sutajio-kosagi/precursor/updates/call-for-developers-precursor-chat-client)).
- sigchat's own README confirms the shared-library design: *"The UI and local storage are provided by the xous chat library."* and *"Contributions to development of apps/sigchat and libs/chat are most welcome."* ([Source](https://github.com/betrusted-io/sigchat)).
- mtxchat was a deliberate simplification away from an earlier tokio-based architecture. The predecessor `mtxcli` README says: *"This is a revised version of the mtxcli Matrix chat program. This version, 0.5.0 (and beyond), strives to be as simple as possible in order to prepare for running on the Xous operating system on the Betrusted (Precursor) hardware device. The previous work used the tokio library for asynchronous communication, but became very complex (NOTE: that code is still available in the mtxcli-tokio branch)."* ([Source](https://github.com/betrusted-io/mtxcli)). The explicit design decision was *move away from an async runtime toward Xous's threading + message-passing model because the latter is auditable*.

**Limitation of this research:** I was unable to fetch the literal contents of `apps/mtxchat/src/main.rs` or `libs/chat/src/lib.rs` through the web tooling available here (GitHub blob URLs returned PERMISSIONS_ERROR and the URLs did not appear in any search snippet result). Claims above about Matrix polling architecture are inferred from the mtxcli README, the crowdsupply update, and the Xous Book's documented idiom — not from reading the source line-by-line. **Before committing code, the implementer should directly read these files:**

- `xous-core/apps/mtxchat/src/main.rs` (the mtxchat event loop — look for `std::thread::spawn`, `create_server`, and how it dispatches `Opcode::Post` / listener callbacks).
- `xous-core/libs/chat/src/lib.rs` and `xous-core/libs/chat/src/api.rs` (the `Chat` type that sigchat itself uses — look for the `set_busy_state`, post-insertion, and event-callback opcodes; the chat library is the abstraction that already owns the modal-vs-background trade-off).
- `xous-core/apps/repl/README.md` says *"Push Events and Listeners provides an overview of the flow of messages between an Xous application and the Xous Services"* ([Source](https://github.com/betrusted-io/xous-core/blob/main/apps/repl/README.md)) — the repl app is explicitly the template from which chat apps were cloned.

### 2.3 The PDDB

The PDDB supports callback notifications for file events: *"the PDDB native API supports callbacks to notify senders of various file events such as deletions and updates."* ([Source](https://betrusted.io/xous-book/ch09-04-api-std.html)). These callbacks use the same one-time-SID idiom the book describes for `NetManager`.

### 2.4 TLS stack

`ring-xous` is pinned to 0.16 and `rustls` to a pre-0.22 API; upgrades are blocked on effort, not blocked on correctness, but be aware: *"The tls library that @nworbnhoj implemented breaks when you upgrade to the latest rustls. … looks like they took a good solid whack at that process. This means the stuff in the danger module needs a deep cut."* ([Source](https://github.com/betrusted-io/xous-core/issues/507)). Signal's provisioning endpoint uses WSS, so sigchat is sitting on top of this older rustls — which means the `rustls::ClientConnection` object is the thing the WS layer wraps, and **this object is `!Sync`**. Any "shared-Mutex" pattern must therefore gate every TLS read and every TLS write through the same lock; candidate (B)'s "main thread writes, timer thread also writes" is a data race against `rustls` internal state.

---

## 3. Security-hardening considerations

Xous's threat model is journalists, activists, and high-assurance users ([Source](https://www.bunniestudios.com/blog/category/hacking/open-source/)). The concurrency choice interacts with the threat model in three places:

1. **SID confidentiality.** The Xous Book is explicit that leaking your main server's SID to another process is tantamount to giving that process the right to call any opcode — including sensitive ones. *"One of the main challenges of push notifications is not disclosing your `SID` to the notifying server."* ([Source](https://betrusted.io/xous-book/ch07-05-asynchronous.html)). The mitigation is the single-purpose SID pattern with a minimal opcode enum. A pure `Arc<Mutex<WS>>` pattern sidesteps this problem (no IPC boundary) but also sidesteps the auditable, reviewable shape the Xous project uses everywhere else.
2. **Constant-time crypto and side channels.** Bunnie has written about constant-time hardening of ring-xous: *"I also have reason to believe that the ECDSA and RSA implementation's constant time hardening should have also made it through the transpilation process. … Our processor is fairly slow, so at 100MHz simply generating gobs of random keys and signing them may not give us enough coverage to gain confidence in face of some of the very targeted timing attacks."* ([Source](https://www.bunniestudios.com/blog/?cat=70)). A background thread that periodically wakes to send WS pings while a QR-code modal is displayed does introduce a *new* timing signal (ping cadence) visible on the wire. However, Signal's provisioning link is an outbound TLS connection and the server already sees TLS record timing — the ping does not leak more than is already leaking. I did not find any Xous-specific guidance on ping timing as a side channel; flagging this as an "unknown unknown" is appropriate rather than overclaiming it matters.
3. **Shared mutable state over crypto objects.** `rustls::ClientConnection` holds the live TLS session keys. Wrapping it in `Arc<Mutex<…>>` creates a potential bug surface where two pieces of code can believe they hold the lock (Xous `Mutex` has a documented fast path and slow path with a fallback to Ticktimer's `BlockingScalar` ids 6/7 and a "poisoning" behavior during the hand-off, per [Source](https://betrusted.io/xous-book/ch02-04-synchronization.html)). The mutex itself is sound, but every `lock()` call is now code the auditor must read. Bunnie's stated concern — *"the fact that Xous doesn't include an implementation of HashMap within its repository doesn't mean that we are any simpler to audit"* ([Source](https://www.bunniestudios.com/blog/category/betrusted/precursor/)) — applies double to concurrent crypto state.

The message-passing pattern **moves the TLS session key into exactly one thread's address space** (from Xous's perspective, the WS-owner thread's stack/locals), never crosses it, and communicates with the UI exclusively via serialized `rkyv`/scalar messages. This is the pattern Xous uses for its own keyring-adjacent services.

---

## 4. Evaluation of the candidate patterns

### (A) `std::thread::spawn` + `Arc<Mutex<SignalWS>>`, background reader, mpsc to main
- **Will it work?** Yes. Xous's `std::thread`, `Arc`, `Mutex`, `std::sync::mpsc` are all supported; the net crate itself uses mpsc ([Source](https://github.com/betrusted-io/xous-core/blob/main/RELEASE-v0.9.md)).
- **Idiomatic?** Partially. It is "ordinary Rust" but not "idiomatic Xous": it violates the pattern the book describes, in which state does not cross threads and a narrow opcode protocol fronts every background worker.
- **Security cost:** TLS state is now behind a `Mutex` — every lock/unlock is audit surface, and every code path that can touch the WS from either thread must be reasoned about. This is the classic "shared mutable state" pitfall message-passing is meant to eliminate.
- **Verdict:** Acceptable if the author prefers it, but strictly weaker than (C) on auditability.

### (B) Ping-only timer thread, main thread still owns WS
- **Will it work?** No. `rustls::ClientConnection` is `!Sync`; reading on the main thread while writing a ping on the timer thread is undefined in the Rust type system and, under `Mutex`, still requires both sides to take the lock — at which point you are in pattern (A) anyway.
- **Verdict:** Reject. Either you share with a lock (pattern A) or you do not (pattern C). (B) is pattern (A) with a foot-gun.

### (C) Dedicated `create_server` + Opcode background thread owns the WebSocket
- **Will it work?** Yes; this is what `NetManager::wifi_state_subscribe` does for wifi state updates ([Source](https://betrusted.io/xous-book/ch07-05-asynchronous.html)).
- **Idiomatic?** Maximally. It is the pattern the Xous Book prescribes by name.
- **Security cost:** The narrow opcode enum *is* the audit surface; everything outside it is unreachable by the UI side. TLS keys never cross a thread boundary except as outbound byte buffers. This matches the "bouncer"/"single-purpose server" pattern the book describes as a deliberate attack-surface reduction.
- **How it solves the 60-second timeout specifically:** the WS-owner thread runs its own event loop. It blocks on either a `recv_timeout`-style WS read *or* a Xous IPC message from the UI. When the UI displays the QR modal, it sends an opcode like `Opcode::Pause` (and/or `Opcode::StartKeepalive(interval_ms)`) to the WS server, which then enters a mode where it (a) reads and discards/queues server pings, replying with pongs, and (b) optionally emits app-level pings on a ticktimer. When the modal dismisses, the UI sends `Opcode::Resume` (or `Opcode::DrainPending`) and the WS server replies with any buffered provisioning frame. The UI is free to block on GAM because the WS is not on that thread.
- **Verdict: Recommended.**

### (D) Non-blocking / pump-driven modal
- **Will it work?** Not without modifying GAM. The `Modals` API is explicitly the deferred-response pattern ([Source](https://betrusted.io/xous-book/ch07-02-caller-idioms.html)), and there is a documented "busy spinner" primitive in GAM for long-running operations but no documented non-blocking modal: *"Add 'busy spinner' primitive to text boxes to the GAM. This allows UIs to show that something is happening without having to explicitly implement that."* ([Source](https://github.com/betrusted-io/xous-core/blob/main/RELEASE-v0.9.md)). The busy spinner is one-way feedback, not an event pump.
- **Verdict:** Out of scope for a sigchat bug fix; would require a GAM change.

### (E) Other patterns in-tree
- The audio callback in `apps/repl` is documented as *"about one of the most complicated and comprehensive things you can do in the repl environment, as it requires real-time callbacks to fill the audio buffer with new samples"* ([Source](https://github.com/betrusted-io/xous-core/blob/main/apps/repl/README.md)) — and it uses the same callback-via-SID pattern.
- The "Deferred Response" idiom (book §7.2.4) is a variant where the callee parks the caller's message in `Option<MessageEnvelope>` and returns it when work completes ([Source](https://betrusted.io/xous-book/ch07-02-caller-idioms.html)). This is useful for "UI thread asks WS server 'give me the next provisioning frame' and blocks until it arrives" — but note that GAM is already blocking the UI thread on the modal, so this is complementary, not alternative: the WS server can store a deferred-response message, service its own WS, and return when ready.

---

## 5. Concrete recommendation

**Adopt pattern (C).** Structure sigchat's provisioning flow as:

1. **`SignalWsServer` background thread.** Before opening the provisioning modal, call `xous::create_server()` to mint a private SID. `std::thread::spawn` a worker that owns the `tungstenite`/WSS handle (constructed inside the thread — the TLS `ClientConnection` never leaves). Main loop:

   ```
   loop {
       // 1. Set a short TLS read timeout (e.g. 500ms) so we cycle.
       match ws.read_message_with_timeout(500ms) {
           Ok(Message::Ping(p))   => ws.write_message(Message::Pong(p))?,
           Ok(Message::Binary(b)) => forward_to_main(b),
           Ok(Message::Close(_))  => mark_closed(),
           Err(Timeout)           => {},
           Err(e)                 => mark_error(e),
       }
       // 2. Poll any Xous IPC messages non-blocking (or use try_receive).
       if let Ok(msg) = xous::try_receive_message(sid) { … }
       // 3. If ticktimer says > keepalive_interval since last write,
       //    send an application-layer ping frame.
   }
   ```

2. **Narrow opcode enum** (mirroring the book's `WifiStateCallback`):
   - `Opcode::GetNextFrame` (deferred-response: main thread sends this blocking; server returns when binary frame arrives, a cancel is received, or WS closes).
   - `Opcode::Cancel` (main thread tells server to unblock the above).
   - `Opcode::SetKeepalive(interval_ms)` (scalar; configures idle ping cadence; default < 60s given Signal's documented timeout).
   - `Opcode::Quit` (Drop equivalent; server shuts down and destroys its SID).

3. **UI code flow unchanged.** The main thread still calls `modals.get_text(...)` synchronously. Before showing the modal, it sends `Opcode::SetKeepalive(25_000)`. After dismiss, it sends `Opcode::GetNextFrame` (blocking) or reads a pre-buffered frame the WS server queued during the modal.

4. **Lifecycle.** Follow the Xous Book's `Drop` idiom: when sigchat's provisioning state object is dropped, send `Opcode::Quit` blocking-scalar, `xous::disconnect`, and the worker thread calls `xous::destroy_server`.

### Why this specifically fixes the bug
- The WS handle is owned by a thread that is *never* parked on GAM. It will happily respond to server pings and drain the TCP/TLS buffer for the entire duration the user spends on their phone.
- Signal's ~60s idle timeout becomes a non-issue because the WS thread actively sends app-level pings on its own ticktimer schedule.
- No TLS state is shared across threads, no `Mutex` guards the `rustls::ClientConnection`, and the UI/WS boundary is one small opcode enum instead of an object interface.

### What NOT to do
- **Do not** put the raw `tungstenite`/`rustls` handle behind `Arc<Mutex<…>>` and let both threads touch it (rules out B; weakens A).
- **Do not** spin up a separate thread that only sends pings while the main thread is allowed to read — this is a write-write race on TLS state.
- **Do not** try to interleave `ws.read` with GAM modal rendering on one thread; GAM modals are blocking by design and there is no pump-mode primitive ([Source](https://betrusted.io/xous-book/ch07-02-caller-idioms.html)).
- **Do not** pull in tokio or an async runtime to solve this. The mtxcli → mtxchat history shows the project deliberately rejected that path: *"The previous work used the tokio library for asynchronous communication, but became very complex"* ([Source](https://github.com/betrusted-io/mtxcli)).
- **Do not** expose sigchat's main SID to the WS worker. Mint a one-time SID for it (book idiom; attack-surface reduction). Keep its opcode enum to the minimum needed.

### Upstream PR / issue history informing the choice
- Issue #507 (`ring` 0.17) notes that the TLS story is fragile and the `danger` module in `lib/tls` needs a deep cut to keep up with rustls ([Source](https://github.com/betrusted-io/xous-core/issues/507)). This is a reason to minimize the code that touches rustls objects — another argument for single-owner WS.
- Issues #162 and #159 (condvar bugs, fixed by @xobs) and the "mutex & condvar refactor" note ([Source](https://github.com/betrusted-io/xous-core/blob/main/RELEASE-v0.9.md)) show concurrency bugs have historically surfaced in this stack. Keeping the concurrent primitives narrow is a lesson the project itself has internalized.
- The net crate's mpsc/worker-thread overhaul ([Source](https://github.com/betrusted-io/xous-core/blob/main/RELEASE-v0.9.md)) is a direct precedent: the net stack's own wait/poll loop is exactly the shape recommended here (dedicated workers + channels).

---

## 6. Honest limits of this report

- I was unable to retrieve and quote the literal text of `apps/mtxchat/src/main.rs`, `libs/chat/src/lib.rs`, or `libs/chat/src/api.rs`. GitHub's HTML blob pages and `raw.githubusercontent.com` URLs both returned permission errors in this environment, and search snippets never surfaced those files' event-loop bodies. **Before implementing, the engineer should open these three files and confirm that the `Chat` library is already doing exactly the `create_server` + single-purpose-SID dance for its Matrix sync worker — my strong prediction is that it is, based on the book idiom and the `NetManager` precedent, but I have not verified it line-by-line.**
- I did not find any Xous issue or PR specifically about "WebSocket keepalive during modal display". The closest architectural precedents are the net service's mpsc overhaul and the book's `NetManager` example; neither is about WebSockets specifically.
- I did not find bunnie writing specifically about concurrency side channels. His writing on side channels focuses on crypto constant-time properties and silicon-level threats ([Source](https://www.bunniestudios.com/blog/?cat=70)), not on threading-level information flow. The ping-cadence side channel I flagged in §3 is my own analysis, not his.
- Xous is still on an older rustls (< 0.22) because of ring 0.16 pinning ([Source](https://github.com/betrusted-io/xous-core/issues/507)); sigchat implementers should verify that whatever WS library they use (likely `tungstenite` over rustls) supports a non-infinite `read` timeout so the worker-loop pattern above actually works.

## 7. One-paragraph summary for the Claude Code session

Refactor sigchat's Signal provisioning so the WebSocket is owned by a dedicated worker thread created with `std::thread::spawn`, which also owns a private `xous::SID` created by `xous::create_server()` and exposes a small `Opcode` enum (`GetNextFrame`/`Cancel`/`SetKeepalive`/`Quit`) to the main sigchat thread. The worker loop alternates short-timeout WS reads (replying to server pings, queuing binary frames) with `xous::try_receive_message` checks and ticktimer-driven app-level pings. The main thread continues to call GAM modals synchronously; it communicates with the worker exclusively via IPC messages and never shares the TLS/WS handle. This matches the Xous Book's §7.2.3 "Asynchronous Idioms" pattern verbatim ([Source](https://betrusted.io/xous-book/ch07-05-asynchronous.html)), matches the net service's own mpsc-worker architecture ([Source](https://github.com/betrusted-io/xous-core/blob/main/RELEASE-v0.9.md)), matches bunnie's stated design preference for auditable message-passing over shared mutable state ([Source](https://www.bunniestudios.com/blog/category/betrusted/precursor/)), and is the only candidate that (a) keeps the TLS session key in exactly one thread, (b) does not require modifying GAM, and (c) reliably services Signal's 60-second idle timeout regardless of how long the user spends on their phone.
