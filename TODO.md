# Follow-up work

Requested on 2026-09-05. Implementation and validation details are in
[Background operation and chat UX](specs/background-and-chat-ux.md).

- [x] **Background operation.** Close to hide, reopen the existing session, explicit Quit, optional hidden startup and a close-behavior setting.
- [x] **Online status.** Visible + focused + activity within five minutes; offline requests when hidden, unfocused or idle; heartbeat/retry, graceful quit, and Ghost-mode override.
- [x] **Pinned messages.** Four-line collapsed preview, full original text on Expand, bounded scrollable expanded panel, and Collapse.
- [x] **Chat-opening speed.** Prioritize history, show bounded account-scoped cached messages before the network response, refresh safely, reuse downloaded media, retain content on failure, and offer Retry.
- [x] **Top-area visuals.** Match sidebar/search and conversation-header heights, center the entry, and adapt chat-search controls to narrow windows.

Live-account confirmation remains useful for the user's reported network delay
and for how another Telegram client displays this session's presence. Tests use
mock data and isolated caches; they do not send real messages or make calls.
