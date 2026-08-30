# Shared Notification Broker Implementation Plan

> **For agentic workers:** use the `subagent-driven-development` skill to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for
> tracking.

**Goal:** Fix [#15](https://github.com/snirt/tmux-agents-mon/issues/15) by replacing one waiting macOS helper process per clickable notification with one on-demand broker that keeps existing notifications clickable until clicked or dismissed.

**Architecture:** Each normal `agents-mon-notifier` invocation becomes a short-lived client. It sends a length-prefixed request to a user-private Unix socket. If no broker is listening, the client starts one detached `--broker` process and retries. The broker holds a non-blocking listener, owns every `mac-usernotifications` response future, runs click commands, and exits when no delivered notifications remain. A `flock` lock permits only one broker and lets its owner safely replace a stale socket.

**Tech Stack:** Rust 2021, `std::os::unix::net`, existing `libc`, existing `mac-usernotifications 0.3.1`.

## Global Constraints

- macOS only; Linux notification behavior stays unchanged.
- No new crate dependency.
- Sidebar closure stops new notification monitoring; already delivered notifications remain clickable.
- At most one waiting `agents-mon-notifier --broker` process per user.
- Socket accepts only bounded, length-prefixed fields and is mode `0600`.
- Preserve existing exit codes: `1` launch/transport, `2` usage, `3` invalid bundle, `4` denied permission, `5` notification failure, `6` click-command failure.
- Broker crash may orphan existing click actions; do not add persistence or a LaunchAgent for this issue.

---

### Task 1: Add bounded broker transport and singleton ownership

**Files:**

- Create: `src/bin/agents-mon-notifier/broker.rs`
- Modify: `src/bin/agents-mon-notifier.rs`

**Interfaces:**

- Consumes: `NotificationRequest { title, body, click }` from the CLI parser.
- Produces:
  - `broker::submit(request: &NotificationRequest) -> std::io::Result<()>`
  - `broker::BrokerOwner::acquire() -> std::io::Result<Option<BrokerOwner>>`
  - `broker::read_request(stream: &mut UnixStream) -> std::io::Result<NotificationRequest>`
  - `broker::write_ack(stream: &mut UnixStream, accepted: bool) -> std::io::Result<()>`

- [ ] **Step 1: Replace tuple payload with a named request and mode parser**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotificationRequest {
    pub title: String,
    pub body: String,
    pub click: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    Setup,
    Broker,
    Notify(NotificationRequest),
}

fn parse(args: &[String]) -> Option<Mode> {
    match args {
        [flag] if flag == "--setup" => Some(Mode::Setup),
        [flag] if flag == "--broker" => Some(Mode::Broker),
        [title, body] => Some(Mode::Notify(NotificationRequest {
            title: title.clone(),
            body: body.clone(),
            click: None,
        })),
        [title, body, click] => Some(Mode::Notify(NotificationRequest {
            title: title.clone(),
            body: body.clone(),
            click: Some(click.clone()),
        })),
        _ => None,
    }
}
```

Delete `--spawned`; detached lifetime now belongs only to `--broker`.

- [ ] **Step 2: Write failing codec and lock tests in `broker.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip_preserves_unicode_and_optional_click() {
        let expected = NotificationRequest {
            title: "Pi finished ✓".into(),
            body: "subject · tmux-agents-mon:1.2".into(),
            click: Some("'agents-mon' 'notification-open' '%12'".into()),
        };
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        write_request(&mut writer, &expected).unwrap();
        assert_eq!(read_request(&mut reader).unwrap(), expected);
    }

    #[test]
    fn request_round_trip_preserves_missing_click() {
        let expected = NotificationRequest {
            title: "AgentsMon".into(),
            body: "ready".into(),
            click: None,
        };
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        write_request(&mut writer, &expected).unwrap();
        assert_eq!(read_request(&mut reader).unwrap(), expected);
    }

    #[test]
    fn request_rejects_oversized_field_before_allocating_it() {
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        writer.write_all(&((MAX_FIELD_BYTES + 1) as u32).to_be_bytes()).unwrap();
        let error = read_request(&mut reader).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn lock_has_only_one_owner() {
        let dir = tempfile_dir("broker-lock");
        let first = BrokerOwner::acquire_at(&dir).unwrap();
        assert!(first.is_some());
        assert!(BrokerOwner::acquire_at(&dir).unwrap().is_none());
        drop(first);
        assert!(BrokerOwner::acquire_at(&dir).unwrap().is_some());
        fs::remove_dir_all(dir).unwrap();
    }
}
```

Implement `tempfile_dir` with `std::env::temp_dir()`, process ID, and an atomic counter; do not add `tempfile`.

- [ ] **Step 3: Run focused tests and confirm failure**

Run: `cargo test --bin agents-mon-notifier request_`

Expected: FAIL because `broker`, codec functions, and constants do not exist.

Run: `cargo test --bin agents-mon-notifier lock_has_only_one_owner`

Expected: FAIL because `BrokerOwner` does not exist.

- [ ] **Step 4: Implement transport with bounded frames**

Use these constants and wire format:

```rust
const MAX_FIELD_BYTES: usize = 64 * 1024;
const NONE_LENGTH: u32 = u32::MAX;
const ACK_ACCEPTED: u8 = 1;
const ACK_REJECTED: u8 = 0;
const SOCKET_NAME: &str = "agents-mon-notifier.sock";
const LOCK_NAME: &str = "agents-mon-notifier.lock";
```

Write `title`, `body`, then `click`; each field starts with a big-endian `u32`. `NONE_LENGTH` is valid only for `click`. Reject larger fields before allocation. `read_request` must use `read_exact`; `write_request` must use `write_all` and `flush`.

`BrokerOwner::acquire_at(dir)` opens the lock file and calls:

```rust
let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
if result == 0 {
    Ok(Some(Self { lock: file, socket_path: dir.join(SOCKET_NAME) }))
} else if io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock {
    Ok(None)
} else {
    Err(io::Error::last_os_error())
}
```

The owner removes a stale socket only after acquiring the lock. Bind the socket, then set permissions to `0o600`. `Drop` removes the socket; closing the file releases `flock`.

- [ ] **Step 5: Implement client submission and startup retry**

`submit` first calls `submit_at(socket_path(), request)`. On connection failure, spawn current executable with `--broker`, null stdio, then retry every 50 ms for at most 5 seconds. A successful request requires `ACK_ACCEPTED`; EOF or `ACK_REJECTED` returns `PermissionDenied`/`InvalidData`, not success.

Only retry connection errors (`NotFound`, `ConnectionRefused`, `ConnectionReset`). Return malformed protocol and permission errors immediately.

- [ ] **Step 6: Run focused tests**

Run: `cargo test --bin agents-mon-notifier`

Expected: all parser, codec, size-limit, and lock tests PASS.

- [ ] **Step 7: Commit**

```bash
git add src/bin/agents-mon-notifier.rs src/bin/agents-mon-notifier/broker.rs
git commit -m "refactor: add shared notifier broker transport"
```

---

### Task 2: Multiplex notification responses in one broker process

**Files:**

- Modify: `src/bin/agents-mon-notifier.rs`
- Modify: `src/bin/agents-mon-notifier/broker.rs`

**Interfaces:**

- Consumes: Task 1 `NotificationRequest`, singleton listener, and request codec.
- Produces:
  - `broker::serve() -> i32`
  - `PendingNotification = Pin<Box<dyn Future<Output = PendingResult>>>`
  - Broker exit when `accepted_any && pending.is_empty()`.

- [ ] **Step 1: Write failing broker-state tests**

Keep native delivery behind one small function seam:

```rust
type PendingNotification = Pin<Box<dyn Future<Output = PendingResult>>>;

struct PendingResult {
    click: Option<String>,
    response: Result<mac_usernotifications::NotificationResponse, mac_usernotifications::Error>,
}
```

Add tests for pure state transitions by constructing ready futures:

```rust
#[test]
fn broker_stays_alive_while_any_response_is_pending() {
    let mut state = BrokerState::default();
    state.accepted_any = true;
    state.pending.push(Box::pin(std::future::pending()));
    assert!(!state.should_exit());
}

#[test]
fn broker_exits_after_last_response_finishes() {
    let state = BrokerState { accepted_any: true, pending: Vec::new() };
    assert!(state.should_exit());
}

#[test]
fn fresh_broker_waits_for_first_request() {
    assert!(!BrokerState::default().should_exit());
}
```

- [ ] **Step 2: Run tests and confirm failure**

Run: `cargo test --bin agents-mon-notifier broker_`

Expected: FAIL because `BrokerState` and pending-response handling do not exist.

- [ ] **Step 3: Implement broker future**

`serve()` must:

1. Validate the app bundle.
2. Acquire `BrokerOwner`; a losing process exits `0` because the winning broker serves clients.
3. Bind a non-blocking `UnixListener`.
4. Enter `mac_usernotifications::block_on_main(BrokerFuture { ... })`.

Keep the existing denied-permission precheck in the short-lived client so it can still return exit code `4`. On the broker's first accepted request, call `noti::blocking::request_auth()` after decoding and acknowledging the request. Cache the result in `BrokerState.authorization: Option<bool>`; denial completes that request without posting and lets the broker exit. This preserves the current detached first-notification permission flow without making the client wait for the user.

`BrokerFuture::poll` must:

- Accept every currently queued connection until `WouldBlock`.
- Set a 250 ms read/write timeout per accepted stream.
- Decode one request.
- Queue its notification future.
- Send `ACK_ACCEPTED` only after the request has been decoded and queued; send `ACK_REJECTED` for malformed requests.
- Poll every pending future with the provided `Context`.
- On default action, run the request click command with `/bin/sh -c`.
- Remove completed, dismissed, and failed futures.
- Return `Poll::Ready(exit_code)` only after at least one request was accepted and no response future remains.

Build each pending future without a timeout so dismissal detection comes from `mac-usernotifications`:

```rust
fn pending(request: NotificationRequest) -> PendingNotification {
    Box::pin(async move {
        let notification = noti::Notification::new()
            .title(&request.title)
            .message(&request.body)
            .sound(noti::sound::GLASS);
        let response = match notification.send().await {
            Ok(handle) => handle.response().await,
            Err(error) => Err(error),
        };
        PendingResult {
            click: request.click,
            response,
        }
    })
}
```

A delivery error completes only that notification; it does not stop responses for other pending notifications.

The main run loop repolls at least once per second; this bounds socket acceptance latency without a polling thread or new runtime.

- [ ] **Step 4: Route CLI modes**

```rust
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match parse(&args) {
        Some(Mode::Setup) => setup(),
        Some(Mode::Broker) => broker::serve(),
        Some(Mode::Notify(request)) => match broker::submit(&request) {
            Ok(()) => 0,
            Err(_) => 1,
        },
        None => {
            eprintln!("usage: agents-mon-notifier [--setup|--broker] | <title> <body> [click-command]");
            2
        }
    };
    std::process::exit(code);
}
```

Retain non-macOS behavior: `--setup`, `--broker`, and delivery return the existing macOS-only usage failure without opening sockets.

- [ ] **Step 5: Run Rust tests and full repository tests**

Run: `cargo test --bin agents-mon-notifier`

Expected: PASS.

Run: `make test`

Expected: PASS, including release packaging checks for `agents-mon-notifier`.

- [ ] **Step 6: Commit**

```bash
git add src/bin/agents-mon-notifier.rs src/bin/agents-mon-notifier/broker.rs
git commit -m "fix: multiplex clickable notifications in one broker"
```

---

### Task 3: Verify bounded processes and document lifecycle

**Files:**

- Modify: `docs/plans/2026-08-06-native-notifications-design.md`

**Interfaces:**

- Consumes: completed shared broker behavior.
- Produces: documented sidebar/broker lifecycle and manual macOS evidence for issue #15.

- [ ] **Step 1: Update design language**

Replace the per-notification 24-hour helper description with:

```markdown
The helper invocation is a short-lived client of one on-demand broker process.
The first notification starts the broker; later notifications submit over a
user-private Unix socket. The broker owns all delivered notification response
handlers, keeps existing notifications clickable after the sidebar closes, and
exits after every delivered notification has been clicked or dismissed. It does
not monitor tmux or produce new notifications after the sidebar closes.
```

- [ ] **Step 2: Build and install the signed helper**

Run: `cargo build --release --bin agents-mon-notifier && make install-app`

Expected: release binary builds, app installs, code signing succeeds, notification permission remains granted.

- [ ] **Step 3: Exercise concurrent notifications**

```bash
helper="$HOME/Applications/AgentsMon.app/Contents/MacOS/agents-mon-notifier"
for n in 1 2 3 4 5; do
  "$helper" "AgentsMon broker test $n" "Dismiss or click this test notification" "true"
done
sleep 3
pgrep -fal 'agents-mon-notifier --broker'
```

Expected: exactly one `agents-mon-notifier --broker` process despite five delivered notifications. No `--spawned` processes exist.

Dismiss four notifications and verify the same one broker remains. Dismiss the final notification and verify within two seconds:

```bash
! pgrep -f 'agents-mon-notifier --broker'
```

Expected: command exits `0` because broker has exited.

- [ ] **Step 4: Verify sidebar closure semantics**

Deliver one real agent-finished notification, close the AgentsMon sidebar, then click the existing notification.

Expected: terminal activates and navigates to the originating pane. No new completion notifications appear after sidebar closure.

- [ ] **Step 5: Re-run all automated checks**

Run: `cargo test && make test`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add docs/plans/2026-08-06-native-notifications-design.md
git commit -m "docs: describe shared notification broker lifecycle"
```
