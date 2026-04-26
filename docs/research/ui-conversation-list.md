# UI Design Research for `sigchat` Conversation-List Screen on Precursor

**Target reader:** software engineer implementing the UI.
**Hardware:** Precursor, 336×536 px monochrome Sharp Memory LCD, physical 11-row-ish QWERTY keyboard + directional pad and a center "Home" select key, no touchscreen, ~16 MiB RAM total for the whole OS with ~4 MiB usable per app.
**Scope:** the screen that lists ongoing 1-to-1 Signal conversations, above the existing `libs/chat` conversation view; 1:1 only for now.

---

## 1. Current Signal UI reference — what the production clients actually do

### 1.1 Android (`signalapp/Signal-Android`)

The row widget is `ConversationListItem` (in `org.thoughtcrime.securesms`). Its code is the clearest specification of Signal's conversation-list row and is reproduced in mirrored form on GitHub. Key constants and behavior:

- Two typefaces are pre-created as class constants and swapped per row based on `unreadCount`:
  ```java
  private final static Typeface BOLD_TYPEFACE  = Typeface.create("sans-serif-medium", Typeface.NORMAL);
  private final static Typeface LIGHT_TYPEFACE = Typeface.create("sans-serif",        Typeface.NORMAL);
  ```
  On bind, both the subject ("last message" preview) and the date get typeface and color changed together:
  ```java
  this.subjectView.setTypeface(unreadCount == 0 ? LIGHT_TYPEFACE : BOLD_TYPEFACE);
  this.subjectView.setTextColor(unreadCount == 0
      ? ThemeUtil.getThemedColor(getContext(), R.attr.conversation_list_item_subject_color)
      : ThemeUtil.getThemedColor(getContext(), R.attr.conversation_list_item_unread_color));
  ...
  dateView.setTypeface(unreadCount == 0 ? LIGHT_TYPEFACE : BOLD_TYPEFACE);
  ```
  The "from" name is also told whether to render itself as "read" via `fromView.setText(recipient, unreadCount == 0);`. ([Signal-Android ConversationListItem.java](https://github.com/oleks/Signal-Android-Phone/blob/master/src/org/thoughtcrime/securesms/ConversationListItem.java) — this is an older mirror but matches the pattern still used today.)

- The unread badge is a separate `ImageView` (`R.id.unread_indicator`) populated via the `TextDrawable` library as a **round filled disc containing the unread count**, with white text on the Signal primary color, 24×24 dp, bold:
  ```java
  unreadIndicator.setImageDrawable(TextDrawable.builder()
      .beginConfig().width(24dp).height(24dp).textColor(Color.WHITE).bold().endConfig()
      .buildRound(String.valueOf(thread.getUnreadCount()),
                  getResources().getColor(R.color.textsecure_primary_dark)));
  ```
  Outgoing threads skip the indicator: `if (thread.isOutgoing() || unreadCount == 0) { unreadIndicator.setVisibility(GONE); return; }`.

- Per-row elements (from the fields of `ConversationListItem`): `AvatarImageView contactPhotoImage`, `FromTextView fromView`, `TextView subjectView` (last-message preview), `TextView dateView`, `DeliveryStatusView deliveryStatusIndicator` (pending/sent/delivered/read check marks for your own outgoing last message), `AlertView alertView` (red "!" for send failure / pending-insecure-fallback), `ThumbnailView thumbnailView` (small attachment preview), `TextView archivedView` ("Archived" pill), `ImageView unreadIndicator`.

- Relative timestamp is rendered with `DateUtils.getBriefRelativeTimeSpanString(...)` — i.e. "2 min", "3h", "Yesterday", "Mon", "Jan 3" style, not absolute clock time.

- **Pinned chats** are a first-class UI concept: tap-and-hold → Pin on Android (or swipe-right on iOS). The pinned rows appear in a "Pinned" section above the rest of the list, are not auto-reordered when a message arrives in them (confirmed by Delta Chat's comparison thread testing Signal Android: "Signal Android also doesn't reorder pinned chats based on activity"), and Signal documents a maximum of up to ~4 pinned conversations on mobile ([Signal support — Pin or Unpin Chats](https://support.signal.org/hc/en-us/articles/360048678832-Pin-or-Unpin-Chats); [Delta Chat forum thread on pinned-chat behavior](https://support.delta.chat/t/dont-reorder-pinned-chats-automatically/3585)).

- **Sort order** below the pinned section is strictly "most recent activity at the top" — the view binds `thread.getDate()` from `ThreadRecord`, and the adapter feed is ordered by thread date descending. When a new message arrives, the Android client re-sorts (`notifyDataSetChanged` / DiffUtil) and the newly updated thread animates to the top, then re-binds with the bold/unread typeface and dot.

- **Mark-as-unread** is user-initiated on both Android (long-press → menu → Mark as unread) and iOS (swipe-right → Unread). After this, a "colored dot" badge is shown on the row ([Signal support — Mark as Unread](https://support.signal.org/hc/en-us/articles/360049649871-Mark-as-Unread); [Business Insider walk-through](https://businessinsider.in/tech/how-to/how-to-mark-messages-as-unread-on-the-signal-secure-messaging-app-for-android-and-iphone/articleshow/78775458.cms)).

- **Pull-to-refresh-as-filter**: On Android, pulling down on the chat list filters the list to only unread chats — an official Signal Mastodon post confirms this ("pull down on your chat list to show *just* your unread messages") ([signalapp@mastodon.world](https://mastodon.world/@signalapp/109718025627619934)).

### 1.2 iOS Signal

- Same per-row elements: avatar, name, preview, timestamp, and an unread badge (Signal iOS uses a filled blue circle with the count). Pinned chats are supported, swipe-right on a row surfaces Pin/Unread icons. The per-row decoration differs cosmetically but the information content matches Android.

### 1.3 Signal Desktop

- Two-pane: left column is the conversation list, right is the currently open conversation. The list uses the same unread dot/count + bold-preview convention.

### 1.4 Takeaways that actually matter for a monochrome small-screen port

From the Signal source we can distil a minimal per-row spec that preserves the "Signal feel":

1. **Primary text: contact display name.** Bold when unread, regular otherwise.
2. **Secondary text: last message preview** (one line, ellipsized). Bold when unread.
3. **Relative timestamp**, right-aligned; bolds with the rest of the row when unread.
4. **Unread badge**: a high-salience shape (filled circle) containing the unread count (typically 1–99+).
5. **Delivery-status glyph** on the last outgoing message (optional for MVP).
6. Pinned rows pinned to the top, never auto-re-sorted among themselves.
7. Sort: pinned first, then all other rows by last-activity timestamp descending.
8. On new incoming message: row moves to top of its section and re-binds bold.

---

## 2. Small-screen non-touch chat patterns — prior art

### 2.1 `mtxchat` and `libs/chat` on Xous (the direct precedent)

`libs/chat` is a Xous-side UI library written by `@nohj` (nworbnhoj / njoh depending on commit) that is shared between `mtxchat` (Matrix) and `sigchat` (Signal). The README for `sigchat` describes the shared model and opcode surface it uses:

> "The Chat library provides the UI to display a series of Signal post (Posts) in a Signal group (Dialogue) stored in the pddb. Each Dialogue is stored in the `pddb:dict` `sigchat.dialogue` under a descriptive `pddb:key` (ie ``). ... `sigchat`'s server is set to receive: `SigchatOp::Post` A memory msg containing an outbound user post, `SigchatOp::Event` A scalar msg containing important Chat UI events, `sigchat::Menu` A scalar msg containing click on a sigchat MenuItem, `SigchatOp::Rawkeys` A scalar msg for each keystroke."
> ([betrusted-io/sigchat README](https://github.com/betrusted-io/sigchat))

Important architectural implications:

- The lib already stores conversations as `Dialogue` objects in the PDDB dict `sigchat.dialogue`. That means **the conversation-list screen is a view over `pddb.list_keys("sigchat.dialogue")` plus the newest `Post` in each `Dialogue`** — the data model is already there; only a view is missing.
- Ops are delivered via scalar messages `Post / Event / Menu / Rawkeys` — the conversation-list screen will be a new state inside the same server loop, dispatching `Rawkeys` either to the list or to the conversation view depending on which is focused.
- The Xous Crowd Supply update announcing the chat framework explicitly calls it "a UI framework for chat clients on Precursor, which can eventually be used to wrap protocols such as Signal or Matrix" ([Xous 0.9.15 release notes](https://www.crowdsupply.com/sutajio-kosagi/precursor/updates/xous-v-0-9-15-release-is-now-available); [Call for Developers: Precursor Chat Client](https://www.crowdsupply.com/sutajio-kosagi/precursor/updates/call-for-developers-precursor-chat-client)).

Note on research limitation: I attempted to pull `libs/chat/src/ui.rs`, `libs/chat/src/lib.rs`, and `apps/mtxchat/src/main.rs` directly via `raw.githubusercontent.com` and the GitHub blob URLs, but the research fetch environment rejected those specific paths because they had not appeared verbatim in search hits. The README text, manifest, and Xous Book graphics chapter quoted below are the most authoritative material I could reach. **When implementing, read those three files directly before finalizing the API**, as the exact enum names below (`SigchatOp::*`) are the current upstream shape.

### 2.2 Xous GAM / graphics primitives you have to work with

From the Xous Book graphics-server chapter:

> "A `TextView` object is a heavy data structure that contains both a `xous::String` and metadata which guides the GAM on how to render the string... One can think of a `TextView` as a text bubble, that can have rounded or square corners, and its content string can be rendered with a selection of options that specify the glyph style (not a font — it's more of a hint than a specifier), alignment, and the size of the text bubble. The text bubble can either be of a fixed size (such that the string will show ellipses ... if it overruns the bubble), or of a dynamically growable size based on its content... `TextView` can both be directly rendered to a `Canvas`, or managed by a secondary object such as a `Menu` or `List` to compose other UI elements." ([xous-core/services/gam README](https://github.com/betrusted-io/xous-core/tree/main/services/gam), also [Graphics Toolkit chapter of the Xous Book](https://betrusted.io/xous-book/ch08-00-graphics.html))

Two consequences for sigchat:

- **You already have variable-height `TextView` bubbles with ellipsis support and fixed-height mode** — that's exactly the primitive a row needs.
- Glyph style is "a hint" — you cannot rely on exact pixel metrics, so the list layout should be measured at runtime from the returned rendered height, not hard-coded.

Recall from the Xous Book: Scalar messages carry 4 × u32, Memory messages carry 4 KiB pages. A list of 20 `{key, last_post_snippet, timestamp, unread_count}` fits comfortably inside a single memory-borrow page (20 rows × ~200 bytes ≈ 4 KiB), which is the obvious transport choice for the list payload.

### 2.3 BlackBerry Messenger / BB email — the classic physical-keyboard precedent

BB devices are the historical gold standard for chat on QWERTY hardware. Documented conventions (from BlackBerry's own docs and `berrydoc.net` for classic OS 7 and earlier):

- **Inbox icons** for message state on the row: distinct glyphs for "Unread message" vs "Read message" vs "Draft" vs "New BBM item". Colorless-friendly glyphs: filled envelope = unread, open envelope = read ([BlackBerry Bold 9930/9900 inbox-icons page](https://blackberry-bold-9930-9900.berrydoc.net/en/quick-help/getting-started-your-smartphone/icons/message-inbox-icons/)).
- **Unread envelope icon with a counter** in the status bar; the "flashing center key" LED/icon is the OS-level "you have something new" indicator — a hardware-level analogue that is inapplicable to Precursor but worth noting conceptually.
- **"Mark-all-read"** shortcut (Alt-I on Gmail-plugin, R on BB native) ([CrackBerry thread on Alt-I](https://forums.crackberry.com/blackberry-os-f298/unread-email-message-icon-wont-go-away-202437/)).
- Bold-for-unread across every BlackBerry email and messaging client. For BBM Enterprise the convention is preserved; BBM also uses an unread counter next to the chat row (from CrackBerry walk-through: "first three options at the bottom are Chats, Contacts, Groups; pulling down reveals filters for Unread" style). Row-level UI is: avatar + display name + last message preview + timestamp + unread count.
- BBM's Enterprise docs additionally document **inline Markdown-style formatting** (`*bold*`, `_italic_`, `~strike~`, `+underline+`) that is locally rendered — useful to know, but not a conversation-list concern ([BBM Enterprise — Start a chat](https://docs.blackberry.com/en/id-comm-collab/bbm-enterprise/latest/macos-bbm-enterprise-user-guide/Chats/amc1438355254792)).

### 2.4 Nokia feature-phone SMS inbox

Documented patterns from Nokia manuals ([Nokia 3230 inbox doc](https://www.manualslib.com/manual/112009/Nokia-3230.html?page=100), [Nokia 500 icons](https://www.manualsdir.com/manuals/190329/nokia-500.html?page=27)):

- Envelope glyph state changes with read status ("indicates an unread text message"). Distinct glyphs for unread multimedia, unread WAP service message, data received via infrared/Bluetooth.
- "When there are unread messages in Inbox, the icon changes to `[filled envelope with superscript]`" — i.e. the inbox parent icon also carries an unread cue.
- Inbox items are typically "sender number/name" + "first few words" + timestamp; unread are bold or marked with an asterisk. ([Alibaba how-to on Nokia messaging](https://www.alibaba.com/product-insights/mastering-nokia-messaging-a-step-by-step-guide-to-reading-texts-on-your-nokia-phone.html))
- Number `1` key from idle = open inbox (single-key shortcut). A similar single-key shortcut on Precursor — e.g. number-key to jump to the Nth conversation — would be idiomatic.

### 2.5 Telegram's official BlackBerry spec

Telegram published a mini-style-guide for its BB10 client that is the cleanest published "monochrome-ish feature-phone chat list" spec in the wild:

> "The chats are sorted chronologically, most recently updated first. The list includes all kinds of chats – group chats, ordinary 1-1 chats and secret chats. Secret chats in this list are always green, and group chats always show the sender's name for the last message ('you' in case the message was sent by the user). Chats with unread messages have an unread counter in a green rectangle. Read incoming messages are not marked in any way. For outgoing messages, three icons are possible: a clock for messages being sent, a single or a double check mark..." ([core.telegram.org/blackberry/chats](https://core.telegram.org/blackberry/chats))

This is essentially the same per-row spec Signal uses. The only two small-screen-specific refinements worth stealing:
- "Read incoming messages are not marked in any way" — i.e. leave maximum whitespace on the 80% case and use visual weight only where it's informative.
- The unread counter is drawn into a small filled rectangle/capsule rather than a circle, which packs slightly better in a narrow row.

### 2.6 Terminal chat clients — keybinding prior art

| Client | Conversation-switching | Unread indicator | Jump-to-anything |
|---|---|---|---|
| **WeeChat** | `Alt+←` / `Alt+→` or `F5`/`F6` for prev/next buffer; `Alt+a` jumps to the highest-activity buffer; `Alt+0..9` direct-select ([WeeChat quick-start](https://weechat.org/files/doc/stable/weechat_quickstart.en.html)) | Per-buffer "activity level" 0–3 (none / text / msg / highlight) rendered as separate colored tags in the status bar ([adv_windowlist.pl](https://github.com/irssi/scripts.irssi.org/blob/master/scripts/adv_windowlist.pl)) | `/buffer name` or `Alt+j` |
| **irssi** | `Alt+←` / `Alt+→`, `Ctrl+n` / `Ctrl+p`, `Alt+0..9`, `Alt+q..o` for 11–19; **`Alt+a` cycles through windows in order of activity level** and only highest-priority ones first ([irssi cheatsheet](https://gist.github.com/tasdikrahman/ec4e46a42cbf38c73ef3af863680c786); [irssi settings](https://irssi.org/documentation/settings/)) | `[Act: 1,3,7]` string in status bar, with colors for data_level = text/msg/hilight/special | `/window goto` |
| **gomuks** (Matrix TUI) | `Ctrl+↑` / `Ctrl+↓` between rooms; `Ctrl+K` opens a fuzzy "jump to room" modal then Tab/Enter to pick ([archiveapp.org/gomuks](https://archiveapp.org/gomuks/)) | Per-room "Display unread message count" + "extra symbol if there are unread notifications or highlights" (the upstream TUI room-list ticket spells this out verbatim: [gomuks issue #551](https://github.com/gomuks/gomuks/issues/551)) | Fuzzy search modal |
| **gurk-rs** (Signal TUI, closest conceptual sibling) | `Ctrl+j` / `Ctrl+k` = select_channel previous/next; `Alt+↑` / `Alt+↓` = select_message; `Alt+C` opens channel modal; `Alt+M` mute; mute-marker is `[M]` in the list ([gurk-rs README](https://github.com/boxdot/gurk-rs)) | Channel list column on left, threads on right; bold/color for unread | `Ctrl+K`-like modal |
| **profanity** (XMPP) | `Alt+0..9` choose window, `Alt+a` jumps to next window "with attention flag"; Readline-based input ([profanity man page](https://manpages.ubuntu.com/manpages/oracular/en/man1/profanity.1.html)) | "attention flag" on window; activity list in status bar | `/win N` |
| **mutt** (mail, not chat, but the classic model) | `j`/`k` in index; `N` mark-as-new; `O` mark-as-old; bold terminal attribute for unread (`color index brightwhite black ~U`) ([mutt-users list on bolding unread](https://mutt-users.mutt.narkive.com/EL77vKug/show-unread-as-bold)) | Single-letter flag column: `N` new, `O` old, `F` flagged, `D` deleted, `r` replied; `o` on a collapsed thread that contains unread | `l limit` pattern |

**Patterns worth adopting for sigchat** (from across all of these):
- A single-letter (or single-glyph) status column is a compact, color-free way to encode state.
- A dedicated key that **jumps to the next conversation with unread activity** is universally loved (mutt-`Tab`, irssi/weechat/profanity `Alt+a`, gomuks `Ctrl+K`).
- Number-key quick-jump to the Nth conversation scales to ~10 conversations cheaply; beyond that a modal or search is needed.

### 2.7 Reticulum / NomadNet and SimpleX

- NomadNet (the original Reticulum CLI messenger) and MeshChat use an Urwid-style split-pane TUI; message rooms look like a list. The recent "thechatroom" page-script for NomadNet explicitly hard-codes `DISPLAY_LIMIT = 28 messages` to fit the MeshChat 2.x browser window and 36 lines for a pure NomadNet page, which is a useful sanity check for Precursor: about **~15 lines of text fit on a 336×536 display** at a typical 24-pixel line height with a 2-row status header and 2-row action footer ([thechatroom repo](https://github.com/fr33n0w/thechatroom)).
- SimpleX Chat's terminal UI is an `ncurses`-style REPL; rooms are presented by name but there is no visual "conversation list" analogue to steal from.

---

## 3. Empty-state and first-run

- **Signal iOS/Android first launch after account registration** jumps you into a blank "Chats" screen with an illustration + "Get started — Invite friends / New chat" call-to-action button. When there are no conversations, Signal does not show the list; it shows a centered onboarding illustration with an actionable FAB.
- **Signal on clean install with zero contacts with the app** sometimes shows a "Find people you know" / "Invite friends" card inside the list area.
- **mtxchat** when no rooms are joined falls through to the menu prompt — the library does not render an empty room list; it pops a modal asking to `/join`.
- **Telegram BB spec** uses a literal inline text string in place of an empty conversation: "Got a question about Telegram?" for the support bot; for other chats, "No messages yet…" ([core.telegram.org/blackberry/settings](https://core.telegram.org/blackberry/settings)).
- **BBM Enterprise** empty-state shows a large centered "Start a chat" button and nothing else.
- **Terminal chat clients** with zero channels just show the input prompt and a help hint (e.g. irssi: status window with "Type /connect or /help"). There is no explicit empty list — the list simply has zero rows.

**Recommendation for sigchat empty state:** a single centered paragraph ("No conversations yet. Link your device or register a number from the menu.") plus a reminder of the Menu key. Avoid an empty sorted list — that looks like an error.

---

## 4. Monochrome display conventions (no color allowed)

Precursor's LCD is 1-bit at 336×536 (Sharp Memory LCD LS032B7DD02, 200 ppi, confirmed: "200 ppi monochrome 336 × 536 px", [Hackaday](https://hackaday.com/tag/precursor/); [Panox Display product page](https://www.panoxdisplay.com/transflective/3-2-memory-lcd-336x536.html)). Every visual cue has to be made with shape, weight, position, and inversion.

Techniques attested in production monochrome UIs:

- **Bold vs regular weight.** This is the Signal pattern (`sans-serif-medium` vs `sans-serif`) and the mutt convention (`color index brightwhite black ~U` is strictly "use the bright/bold attribute on unread"). In a 1-bit font rasterizer, bold is achieved by running a stroke-widen or by shipping two font faces; Xous ships `regular` and `bold` glyph sets inside the graphics-server's font assets, accessible via the `TextView` glyph-style "hint".
- **Inversion.** Black background with white text for a selected row is the single most legible selection indicator on a 1-bit display. The early Kindle UI, old BlackBerry menu lists, and the Pebble menu list all use this exclusively for the selected row. It reads well in bright sun and does not require color.
- **Leading glyph.** BlackBerry's inbox uses a single-character leading glyph (filled envelope / open envelope / paperclip) to encode state in a color-free way. Plausible Precursor glyphs (all available in any reasonable 1-bit bitmap font):
  - `●` (filled circle) for "has unread messages"
  - `○` (open circle) or space for "read"
  - `▸` (right-pointing triangle) for "selected" / "focused"
  - `!` or `⚠` for send-failed
  - `✓` / `✓✓` for sent / delivered on the last outgoing message
- **A counter in a rectangle/box.** Telegram's BB spec puts the unread count inside a filled rectangle. On a monochrome display a small filled rectangle with white numbers inside is an exceptionally clear "unread badge" that maps cleanly to Signal's blue-circle badge.
- **Horizontal rules.** A single thin rule between rows (`─`, 1-px) is the Pebble/Kindle convention and it avoids visual crowding without adding text weight. Using ruled lines as row separators is also important because the Precursor's LCD has no anti-aliasing hint at rendering boundaries — whitespace-only separators read as bleed.
- **Right-alignment as semantic.** Timestamp right-aligned, everything else left-aligned, is the one piece of information-density the Signal layout relies on that also works with a bitmap font and no kerning metrics.
- **Whitespace buffers.** The Nielsen Norman Group's Kindle Content Design critique is relevant: a linear list with one-line-per-item scales much further than a grid when the display is small ([NN/G — Kindle Content Design](https://www.nngroup.com/articles/kindle-content-design/)). With ~15 lines on screen, a single-line-per-conversation list gives you 12–13 conversations visible without scrolling, which is enough for most users.
- **Pebble/e-ink wisdom** (summary from Smashing/ProtoPie/Yanko for Pebble-class monochrome): use bold high-contrast typography; iconography should be literal, not abstract; minimum font sizes must be preserved — and crucially, **don't try to convey hierarchy with color; use size, position, and weight only**.

Practical "monochrome UI style book" for sigchat:

| Semantic | Monochrome encoding |
|---|---|
| Unread row | Bold name + bold preview + leading `●` + trailing count badge (filled rect) |
| Read row | Regular weight, no leading glyph |
| Selected row (focus) | Full-row inversion: black bg, white fg; bold/weight unchanged |
| Pinned | Leading `▪` pin glyph in name column; grouped in a separate top block separated by a double rule `═` |
| Muted | `♪̸` or plain `M` suffix next to name (gurk's `[M]` convention) |
| Send-failed (outgoing last msg) | Trailing `!` glyph in the row, right-aligned |
| Sent / delivered | `✓` / `✓✓` next to timestamp |
| Row separator | Single-pixel horizontal rule |
| Section separator (Pinned/Chats) | Double-pixel rule + section header in inverted small-caps |

---

## 5. Navigation model for keyboard-only, small-screen

### 5.1 What Precursor actually has

From the Xous wiki and Crowd Supply docs:
- Physical QWERTY on-device with 4 arrow keys and a physical center **Home** key that is the universal "select/enter" / OK.
- The hosted emulator maps `Home` to `Fn+←` on Mac ([betrusted-wiki home](https://github.com/betrusted-io/betrusted-wiki/wiki)).
- There is **no dedicated Back key in hardware**; back-navigation is app-defined.
- No function row. There is no `F1`/`F2`/... on Precursor — this contrasts with terminal clients. You get arrows, Home, printable characters, and a few modifiers.
- Key-event stream is delivered to chat apps as `SigchatOp::Rawkeys` scalar messages (one per keystroke, as stated in the sigchat README).
- `vault` uses `F1..F4` on keyboards that have them (from the v0.9 release notes: "lefty mode flips deny key from F1 to F4"), but since Precursor's keyboard has no dedicated F-keys, those correspond to remapped top-row keys — treat as "menu slot 1..4".

### 5.2 Recommended navigation model

From the terminal-client landscape the three features that matter most and are cheap to implement:

1. **`↑` / `↓` move focus within the conversation list**, wrapping at top/bottom.
2. **`Home` opens the focused conversation** (push the conversation view over the list; the list state is preserved).
3. **A "back" convention**: the existing menu key (the three-dot/menu button that `libs/chat` already consumes to display the app menu) should, when inside a conversation view, offer a "Back to chats" item. The simplest additional binding is **`←` (left arrow) = back out of the conversation view** to the list when the text input is empty (if the input has text, `←` moves the cursor). This mirrors most feature-phone conventions and avoids needing a dedicated back key.
4. **Jump-to-unread**: a single-key shortcut to cycle through conversations with `unread > 0`, starting from the most recent. Irssi / weechat / profanity all bind this to `Alt+a`; on Precursor with no Alt, the most idiomatic mapping is a **single printable letter when the list has focus** — e.g. `n` = next unread. This works because the conversation-list screen has no text entry; rawkeys can be consumed as commands.
5. **Number-key quick-jump**: `1..9` and `0` jump to the 1st–10th conversation in the list, à la irssi/weechat `Alt+0..9` and Nokia `1` = inbox. With ≤20 conversations this covers everything; with 11+ conversations, user starts over at 1 to 10 after scrolling.
6. **`→` / Home inside the list** both open the focused conversation (so users who expect "right = go deeper" feel at home).
7. **`PgUp` / `PgDn` equivalents**: Precursor does not have PgUp/PgDn; bind `Shift+↑` / `Shift+↓` to jump by screenful.

### 5.3 "Back" without a back button

Two published conventions resolve this cleanly:

- **Modal stack with Menu-item escape**, the Xous convention used by `modals` and `vault`: the app menu always has a "Close" / "Back" entry that pops the view. This works but requires an extra keypress.
- **Left-arrow-when-empty = back**, the BlackBerry and Nokia convention. The left arrow is already used for cursor movement inside a text input; hijacking it only when the input is empty is the standard compromise and is used in every BlackBerry Messenger client. Combine with Menu → Back as the discoverable backup.

### 5.4 Key routing summary (proposed)

| Context | Key | Action |
|---|---|---|
| Conversation list | `↑` / `↓` | Move focus |
| Conversation list | `Shift+↑` / `Shift+↓` | Page up / page down |
| Conversation list | `Home`, `→`, `Enter` | Open focused conversation |
| Conversation list | `n` | Jump to next conversation with unread |
| Conversation list | `1..9`, `0` | Jump to conversation 1..10 |
| Conversation list | `u` | Toggle mark-unread on focused row |
| Conversation list | `p` | Toggle pin on focused row |
| Conversation list | Menu key | Open app menu (New chat, Settings, Link device, Register) |
| Conversation view (empty input) | `←` | Back to conversation list |
| Conversation view | all other keys | Existing `libs/chat` behaviour unchanged |

---

## 6. Memory footprint

Key facts:

- Precursor has **16 MiB RAM** total; per the Xous apps README, **"As of Xous 0.9.6, the OS takes up about half the available RAM (8 out of 16 MiB)... For smooth operation of the PDDB and other resources, we suggest leaving about 4 MiB of free space, so apps should try to stay within the range of 4 MiB in size. Note that as of Xous 0.9.6, code is copied into RAM from FLASH, so a large portion of the RAM usage is actually the code."** ([xous-core/apps/README.md](https://github.com/betrusted-io/xous-core/blob/main/apps/README.md)).
- Baochip (the future, tighter target) has **2 MiB RAM total and 4 MiB FLASH** ([Xous 0.10.0 Baochip release](https://www.crowdsupply.com/sutajio-kosagi/precursor/updates/xous-0-10-0-introducing-baochip-1x-support)). This makes the "flat list view over PDDB records" approach mandatory — any design that preloads whole `Post` histories into RAM to populate previews will not fit.
- Encrypted swap exists on Precursor but the design goal is still fit-in-RAM; swap is a hypothetical "Betrusted" feature, and each swap page costs a secure-IPC round-trip.

Memory cost tradeoffs for the conversation-list screen:

| Choice | RAM cost | Recommendation |
|---|---|---|
| Keep all `Dialogue` records open in RAM | O(N × message-count × avg-size) — unbounded | **No.** Don't do this. |
| Keep list of `{dict_key, last_post_timestamp, last_post_snippet≤80 bytes, unread_count, pinned_bool}` in RAM | ~120 bytes × 20 conversations ≈ 2.4 KiB | **Yes.** This is the canonical design. |
| Render rows lazily from PDDB on each redraw | CPU cost, no RAM cost | Acceptable on Precursor (15 lines × one PDDB lookup per row ≈ tens of ms) — do this on first entry and cache. |
| Keep avatars in RAM | O(N × glyph bytes) | Drop avatars entirely for v1. Signal's avatar is colorful and does not monochromize well, and a monochrome display with no hash-derived glyphs will look uglier than "no avatar". Use a single-letter initial in a bordered box if anything, generated on render from the name. |
| Preload unread counts at PDDB mount | one u32 per Dialogue cached in PDDB key metadata | **Yes.** Store `last_post_ts` and `unread` as separate tiny PDDB keys so you don't have to scan the full Dialogue to compute the list. |

A reasonable working-set budget: **≤8 KiB for the conversation-list data model**, **≤32 KiB for the screen's frame buffer + working glyph cache**, and **zero extra long-lived allocations** when the list is not on-screen. This leaves the full 4 MiB budget for `libsignal` and TLS state, which is where the real cost will be.

The reference question was "how much does mtxchat's UI cost in runtime memory?" — I could not find a published figure. The Xous analyze-bloat script ([analyze-bloat.sh](https://github.com/betrusted-io/xous-core/blob/main/analyze-bloat.sh)) is what you should run locally to answer that; it iterates each service and app and reports `release` binary size and heap profile. Mtxchat's PDDB storage model is identical to what we propose (one PDDB dict per Dialogue, one key per Post).

---

## 7. Concrete design recommendation for the sigchat conversation-list screen

### 7.1 Screen layout (336 × 536, monochrome)

```
┌─────────────────────────────────────────────────┐  ← 1-px rule
│ sigchat                     [wifi]  14:32   ▣12 │  ← 24-px status bar: app name,
├─────────────────────────────────────────────────┤    net state, clock, total-unread
│ ▪ Alice Nguyen                     2m         ● │  ← Pinned section header implicit
│   "sure, meet at 6"                           3 │    by ▪ prefix. Bold = unread.
├─────────────────────────────────────────────────┤
│ ▸ Bob Kowalski                    12m         ● │  ← ▸ = currently focused (also
│   "did you get the file?"                     1 │    whole row inverted)
├─────────────────────────────────────────────────┤
│   Carol Whitfield                  1h           │  ← Regular weight = read
│   Thanks!                                       │
├─────────────────────────────────────────────────┤
│   Dad                             Yesterday     │
│   ✓✓ "On my way"                                │  ← last msg was outgoing+delivered
├─────────────────────────────────────────────────┤
│   +1 415 555 0199                 Mon           │  ← unknown contact → E.164
│   "Your Uber has arrived"                       │
├─────────────────────────────────────────────────┤
│                                                 │
│                      ⋮                          │  ← remaining rows
│                                                 │
├─────────────────────────────────────────────────┤
│  ↑↓ Select   Home Open   n Next unread   ☰ Menu │  ← persistent hint footer, 20-px
└─────────────────────────────────────────────────┘
```

Each row is **48 px tall** (two lines of ~22-px bold-capable type + 4 px padding). At 336 × 536, subtracting the 24-px status bar and 20-px hint footer leaves **492 px / 48 = 10 full rows visible**, with a half-row preview of the next item so the user knows there's more below. Number of items is unbounded; list scrolls.

**Row fields** (left to right):
1. 12-px-wide "state column":
   - `▸` if this row has focus (in addition to row inversion)
   - `▪` if pinned (overrides `▸` visually but row is still inverted when focused)
   - blank otherwise
2. Display name, left-aligned, bold if `unread_count > 0`, truncated with `…` at ~60% of row width.
3. Trailing zone (right-aligned):
   - Brief relative timestamp ("2m", "14:32", "Mon", "Mar 3") — bold when unread.
   - Unread badge: `●<n>` where `<n>` is the count, rendered inside a ~28-px-wide filled rectangle for counts ≥ 1, omitted otherwise. For `n ≥ 100`, render `99+`.
4. Second line: last-message preview, ellipsized, bold when unread. Prefix with `✓`/`✓✓`/`⏱`/`!` if the last message is outgoing (sent / delivered / pending / failed).

### 7.2 Data model

```rust
// libs/chat/src/list.rs (proposed new file)
pub struct DialogueSummary {
    pub pddb_key: String,          // key inside the sigchat.dialogue dict
    pub display_name: String,      // ≤ 32 chars typ.
    pub last_post_snippet: String, // ≤ 80 chars, already ellipsized
    pub last_post_ts: u64,         // UNIX ms; used for sort order
    pub last_post_outgoing: bool,
    pub last_post_status: PostStatus, // Pending | Sent | Delivered | Read | Failed
    pub unread_count: u32,
    pub pinned: bool,
    pub muted: bool,
}

pub enum ListEvent {
    DialogueUpdated(String),   // pddb_key of the Dialogue whose state changed
    DialogueDeleted(String),
    ListShown,
    ListHidden,
}
```

- Persist `last_post_ts`, `unread_count`, `pinned`, `muted` in **separate small PDDB keys per Dialogue** (e.g. under `sigchat.meta` dict keyed by the same dialogue key). Do not scan all Posts at list-render time — that is the single biggest perf trap.
- On startup, walk `sigchat.meta` (fast; one scalar per Dialogue), build the `Vec<DialogueSummary>`, and sort: `(pinned desc, last_post_ts desc)`.
- On `SigchatOp::Post`, update the corresponding `DialogueSummary` in place, re-sort (partial re-insert — the updated row always floats to either the top of the pinned section or the top of the unpinned section), and redraw.
- The list view should **never allocate per-row on redraw**; `TextView` objects for each visible row are pooled and re-bound.

### 7.3 Integration into `libs/chat`

The existing `libs/chat` already exposes a single-conversation view. Add a new public state machine:

```rust
pub enum ChatScreen {
    List,            // NEW: conversation-list screen
    Conversation,    // existing
    Menu,            // existing
}
```

- Add a `Chat::open_list()` API that `sigchat`'s server calls after PDDB mount.
- When `Chat` is in `List` and receives a `Rawkeys`:
  - arrows / Home / n / 1..9 / u / p — handled locally (see §5.4).
  - printable text — ignored (no quick-search in v1; v2: type-to-filter list by name prefix, à la Kindle).
- When `Chat` is in `Conversation` and receives a left-arrow with empty input, it transitions back to `List`, re-rendering from the cached summary vector.
- On `SigchatOp::Event`: if the event updates a Dialogue that is not currently open in the conversation view, the list should still increment its cached `unread_count` and `last_post_ts`.

### 7.4 Empty-state behavior

When `Vec<DialogueSummary>` is empty on entering `List`:
- Center-align a two-paragraph message:
  - Line 1: "No conversations yet."
  - Line 2: "Press Menu to link a device or register a phone number."
- Omit the scrollable list entirely; hint footer reduces to `☰ Menu`.

### 7.5 New-conversation entry point

The secondary concern of "send a first message to someone who has never messaged you" is routed through the menu (`libs/chat::Menu`), with a new item `"New chat"` that either:
- pops a contacts-modal listing PDDB-stored contacts (post-MVP), or
- asks the user to enter an E.164 phone number via the existing `modals` text-input primitive (MVP).

### 7.6 What to skip in v1 (MVP)

- Avatars (monochrome avatars without color are a visual distractor; skip).
- Group rows (scope explicitly deferred).
- Swipe actions (no touchscreen).
- Mark-all-read (easy to add later via menu).
- Typing indicator / read-receipt animations (high cost, low signal on a refresh-on-event LCD).
- Archive view (Signal has it; defer).

### 7.7 Sketched minimal Rust row renderer (pseudocode against Xous GAM)

```rust
fn draw_row(canvas: &Canvas, y: i16, row: &DialogueSummary, focused: bool) {
    let row_rect = Rectangle::new(0, y, 336, 48);

    if focused {
        canvas.fill(row_rect, Color::Black);  // invert background
    }
    let fg = if focused { Color::White } else { Color::Black };

    // State-column glyph
    let state_glyph = if row.pinned { '▪' } else if focused { '▸' } else { ' ' };
    canvas.draw_glyph(4, y + 4, state_glyph, fg, GlyphStyle::Regular);

    // Display name
    let name_style = if row.unread_count > 0 { GlyphStyle::Bold } else { GlyphStyle::Regular };
    let mut name_tv = TextView::new(&row.display_name, name_style, fg);
    name_tv.set_bounds(16, y + 2, 210, 24);
    name_tv.set_ellipsize(true);
    canvas.draw_textview(&name_tv);

    // Timestamp
    let ts_str = brief_relative(row.last_post_ts, now_ms());
    let mut ts_tv = TextView::new(&ts_str, name_style, fg);
    ts_tv.set_bounds(228, y + 2, 70, 22);
    ts_tv.set_align(Align::Right);
    canvas.draw_textview(&ts_tv);

    // Unread badge
    if row.unread_count > 0 {
        let count_str = if row.unread_count >= 100 { "99+".into() }
                        else { row.unread_count.to_string() };
        let badge_w = 28;
        let badge_rect = Rectangle::new(336 - badge_w - 4, y + 24, badge_w, 20);
        canvas.fill(badge_rect, fg);  // invert within badge
        let mut n_tv = TextView::new(&count_str, GlyphStyle::Bold,
                                     if focused { Color::Black } else { Color::White });
        n_tv.set_bounds_from_rect(badge_rect);
        n_tv.set_align(Align::Center);
        canvas.draw_textview(&n_tv);
    }

    // Preview (second line)
    let preview_prefix = match (row.last_post_outgoing, row.last_post_status) {
        (true, PostStatus::Pending)   => "⏱ ",
        (true, PostStatus::Sent)      => "✓ ",
        (true, PostStatus::Delivered) => "✓✓ ",
        (true, PostStatus::Failed)    => "! ",
        _ => "",
    };
    let preview = format!("{}{}", preview_prefix, row.last_post_snippet);
    let mut p_tv = TextView::new(&preview, name_style, fg);
    p_tv.set_bounds(16, y + 24, 290, 22);
    p_tv.set_ellipsize(true);
    canvas.draw_textview(&p_tv);

    // Row separator
    canvas.draw_hline(0, y + 47, 336, fg);
}
```

Exact API names (`Canvas::fill`, `TextView::set_ellipsize`, glyph constants) should be matched against what `services/gam` and `services/graphics-server` actually export; the Xous graphics chapter confirms these capabilities exist but the concrete method names on your branch may differ.

---

## 8. Honest uncertainty flags

- I was unable to read `libs/chat/src/*.rs` and `apps/mtxchat/src/*.rs` directly in this session — the research fetch environment could not dereference those specific GitHub paths. **Before writing code, read them and verify:** (a) the exact opcode names in `libs/chat` (`ChatOp`? `ChatUiOp`?), (b) whether the library currently exposes a "list of Dialogues" enumeration API or whether you have to add one, (c) whether `libs/chat` already has its own screen-state enum that you should extend rather than add to, (d) the exact keycode enum coming off `Rawkeys`.
- The claim that Precursor has no function-row keys is based on the Xous wiki's description of physical keys and the absence of any `F1`/`F2` bindings in `apps/*` that I could find in search snippets; `vault`'s "F1"/"F4" naming refers to numeric-position top-row soft keys, not literal PC function keys. Verify against the keyboard service (`services/keyboard`) before committing to the `1..9` quick-jump scheme — if those positions collide with vault conventions users already know, you may want to use letter keys only (`j/k/n/p/u`) instead.
- mtxchat's actual runtime memory footprint is not published anywhere I could find. Run `analyze-bloat.sh` on your branch and budget accordingly.
- Signal's exact current behavior when a new message arrives in a *pinned* conversation (does the pinned section re-sort internally, or is the pinned order fixed?) is inconsistent across sources: the Delta Chat testing notes say "Signal Android... doesn't reorder pinned chats based on activity", while Signal's own support page is silent on the internal order of the pinned section. The **conservative, Signal-consistent, and least-surprising** choice is to preserve pin-order (i.e. the order in which pins were added), and only re-sort the un-pinned section by `last_post_ts`.
- Some of the terminal-client keybindings (e.g. gurk-rs, profanity) are configurable and I quoted defaults. Your users may rebind; design around `n`/`u`/`p` as sigchat-specific defaults rather than inheriting any one client's exact bindings.

---

## TL;DR for the implementer

1. Add a new screen state `ChatScreen::List` to `libs/chat`.
2. Model the list as `Vec<DialogueSummary>` kept hot in RAM (≤ 3 KiB for 20 rows), sourced from a new `sigchat.meta` PDDB dict of small per-Dialogue metadata keys (last_post_ts, unread_count, pinned, muted).
3. Sort: `pinned desc, last_post_ts desc`. Pinned group preserves pin-add order; un-pinned group re-sorts on each `SigchatOp::Post`.
4. Per-row visual encoding (no color anywhere): bold-for-unread (Signal convention), leading `●` + trailing filled-rect count badge for unread, full-row inversion for focus, `▪` prefix for pinned, 1-px horizontal rule between rows. Row height 48 px; 10 visible rows + status bar + hint footer.
5. Keyboard: `↑`/`↓` focus, `Home`/`→`/`Enter` open, `n` = next unread, `u`/`p` mark-unread/pin, number keys for quick-jump, Menu key for New-chat / Register / Link-device, left-arrow-when-input-empty for back-from-conversation.
6. Empty state: centered two-line message pointing at the Menu key; no empty scrollable list.
7. Skip avatars, groups, archive, swipes, typing indicators in MVP.
8. Budget ≤ 8 KiB RAM for the list view's state; never cache full `Dialogue` contents at list level.

Authoritative artifacts cited repeatedly above: [Signal-Android ConversationListItem.java (mirror)](https://github.com/oleks/Signal-Android-Phone/blob/master/src/org/thoughtcrime/securesms/ConversationListItem.java), [sigchat README](https://github.com/betrusted-io/sigchat), [Xous apps README (4 MiB app budget)](https://github.com/betrusted-io/xous-core/blob/main/apps/README.md), [Xous GAM graphics chapter](https://betrusted.io/xous-book/ch08-00-graphics.html), [Telegram BB chat-list spec](https://core.telegram.org/blackberry/chats), [gomuks TUI room-list issue #551](https://github.com/gomuks/gomuks/issues/551), [gurk-rs README + keybindings](https://github.com/boxdot/gurk-rs), [Signal support — Pin or Unpin Chats](https://support.signal.org/hc/en-us/articles/360048678832-Pin-or-Unpin-Chats), [Signal support — Mark as Unread](https://support.signal.org/hc/en-us/articles/360049649871-Mark-as-Unread), [Delta Chat forum — pinned-chat ordering in Signal](https://support.delta.chat/t/dont-reorder-pinned-chats-automatically/3585), [Crowd Supply — Xous 0.10.0 Baochip 2 MiB RAM note](https://www.crowdsupply.com/sutajio-kosagi/precursor/updates/xous-0-10-0-introducing-baochip-1x-support), [Hackaday — Precursor 336×536 200ppi display](https://hackaday.com/tag/precursor/).
