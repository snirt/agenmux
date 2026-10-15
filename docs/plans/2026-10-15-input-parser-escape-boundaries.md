# Consume complete escape input before dispatch (#205)

## Goal

Prevent terminal escape tails and bracketed paste from becoming commands outside
text fields. Implementation and verification completed; results below.

Issue: https://github.com/snirt/agenmux/issues/205

## Findings

- `RawMode::enable` enables bracketed paste for the entire popup.
- `escape_chord` recognizes short CSI/SS3 forms but returns after a mismatching
  byte. For `ESC[1;5A`, it leaves `5A` unread. For `ESC[200~`, it leaves `0~`
  and the paste body unread.
- `read_key_with_config` passes those remaining bytes through normal action and
  sequence dispatch. Paste can therefore invoke `x`, `u`, `R`, Enter, or `dd`.
- `read_search_key` separately probes for the paste prefix, then replays its
  lookahead through `chord`. It shares the escape decoder and must not regress.
- Existing `read_paste` recognizes the exact end marker, caps retained text at
  65,536 bytes, continues draining oversized input, and uses a one-second idle
  timeout. Normal key tails use 50ms; search tails use 100ms.
- Runtime entry points: `sidebar::event_loop` selects normal versus text input;
  `pane_writers::run_pane` uses `read_search_key` to relay literal text. Framed FIFO
  logical keys also call `decode_protocol_payload` with a bounded payload reader.

## Minimal design

Keep changes inside `src/input.rs`, including its existing test module. No new
module, dependency, configurable key binding, or public protocol change.

1. Consume a complete CSI tail before recognizing a key: parameters
   (`0x30..=0x3f`), intermediates (`0x20..=0x2f`), then one final byte
   (`0x40..=0x7e`). Unknown complete sequences produce no action. Keep capture
   bounded while draining unsupported long sequences through their final byte.
2. Recognize only currently supported unmodified arrow, Home/End, Delete, and
   PageUp/PageDown forms. Preserve SS3 cursor mode and bare Escape. Modified
   arrows and function-key sequences remain unsupported and ignored, not
   downgraded to plain arrows.
3. Distinguish paste-start recognition from a key chord using a small private
   parse result. Do not add paste to `KeyChord` or expose it as a bindable action.
   Share escape recognition between normal and text readers rather than keeping
   the search reader's separate partial-prefix probe.
4. At the raw input boundary, route `ESC[200~` to shared paste consumption:
   discard body in normal mode and return `Key::Other`; collect body in text
   mode and return one `Key::Text`. Reuse the existing exact end-marker matcher,
   text limit, and paste timeout. Discard mode need not retain body text.
5. Stop exactly after `ESC[201~`. The next real key remains unread and works.
   Keep FIFO framing intact: decoding a bounded logical-key payload must never
   drain beyond that payload into another packet or its client identity.
6. Preserve existing EOF and incomplete-tail behavior. Explicitly test timeout
   and malformed input handling; do not claim arbitrary delayed tails or a
   missing paste terminator are solved by a stateless timed reader. If those
   cases require persistent discard state, surface that scope before expanding
   this fix.

## Implementation sequence

### 1. Add failing boundary tests

Use existing pipe-backed tests and runtime entry points, not only a parser helper.

- Normal mode: feed `ESC[200~xuR\rddESC[201~j`. First read must be `Key::Other`;
  next read must be `Key::Down`; input must then be exhausted. This proves no
  command key or sequence prefix escaped the paste.
- Feed `ESC[1;5Aj`. Expect `Key::Other`, then `Key::Down`, then no pending bytes.
- Table-test other modified arrows, multi-digit tilde function keys, CSI with
  intermediate bytes, and an unsupported CSI final. Assert full consumption
  without swallowing a following real key.
- Cover empty paste, embedded Escape/newline/control bytes, marker-like body
  fragments, and a payload larger than the text retention limit. Stream large
  input from a writer thread so tests cannot deadlock on pipe capacity.

### 2. Fix shared escape recognition and paste routing

Implement the design above; update direct `escape_chord` helper tests as needed.
Preserve exact byte-reader ownership across normal input, text input, and framed
protocol decoding. Add assertions that recognized sequences stop at their final
byte and that framed ignored input leaves the next packet intact.

### 3. Verify text and existing input behavior

- Text mode: same command-like paste remains one literal edit, including Unicode;
  next typed key remains separate. Retain existing cursor/edit-key tests.
- Text mode modified CSI: ignored as one input, never inserts trailing `5A`.
- Preserve CSI/SS3 arrows, Home/End variants, Delete, PageUp/PageDown custom
  bindings, bare Escape reset/cancel, normal printable commands, emergency exit,
  and click/owned-key/text FIFO packets.
- Use chunked writes below existing tail timeouts to exercise in-flight bytes.
  Test incomplete input without hanging; document timeout recovery limits.

## Verification commands

Run from the worktree, under explicit Bash:

```bash
/bin/bash -c 'cargo test --locked input::tests'
/bin/bash -c 'cargo fmt --check && cargo test --locked'
/bin/bash -c 'bash tests/run.sh'
```

Before changing code, run focused tests as a baseline; verify new regressions
fail on the old parser and pass after the fix.

## Live verification

Use a scratch tmux socket/session and disposable targets, never real panes with
valuable state. Build and launch the worktree binary explicitly. Send literal
bracketed-paste and modified-arrow bytes to its popup/TTY input path, not only
`agenmux key` logical actions. Confirm normal-mode paste performs no command,
modified arrows perform no navigation, and a subsequent plain key works. Enter a
text field and confirm paste still inserts literal text atomically. Capture only
sanitized evidence if anything is retained.

This is an input-parser bug, not an agent-status detection change; detector
fixtures and production daemon/config changes are outside scope.

## Completion and limits

- Issue acceptance passes through `read_key_with_config` with normal keys.
- Supported input and text paste behavior stay unchanged; both test suites pass.
- Review exact diff and `git status`; scan added/changed content for credentials,
  private identifiers, raw captures, and local paths before handoff.
- No release bump, install, production restart, commit, or PR during planning.
- Unbracketed paste cannot be distinguished from typed commands. Missing end
  markers and bytes delayed beyond existing timeouts remain explicit risks to
  evaluate, not silently broadened acceptance claims.

## Verification results

- Existing 11 input tests passed before edits. New normal-paste and modified-CSI
  regressions both failed on the old parser, then passed with the fix.
- Input coverage now includes 21 tests: command-like/empty paste, CSI parameters
  and intermediates, oversized paste and retention-limit edges, framed FIFO
  boundaries, EOF/timeouts, long CSI tails, and chunked delivery.
- `cargo fmt --check`, `cargo test --locked`, and `bash tests/run.sh` passed.
  The existing lifecycle latency diagnostic remains intentionally ignored.
- Active source diagnostics reported no warnings or errors.
- Live scratch tmux pane ran the worktree release binary's `sidebar` TTY path.
  Literal normal-mode paste and Ctrl-Up left the screen unchanged and emitted
  only `Key::Other`; the next `j` navigated. Search paste inserted one literal
  edit; Ctrl-Up inserted nothing; a following typed character worked. All scratch
  targets survived. Scratch server was stopped; production runtime was untouched.
- Paste retention now trims end-marker bytes correctly even when the body is
  within five bytes of the text limit. No raw captures or traces were added.
- Existing timeout policy is retained and tested, not hardened: after an idle
  timeout, subsequent bytes are fresh input. Unbracketed paste and slow or
  unterminated streams are not covered by the isolation guarantee.

## Interrupted escape follow-up

- An interrupted CSI/SS3 tail must not consume the next ESC as ordinary data:
  `ESC[1;ESC[A` previously ended at the second `[` and leaked `A`.
- Restart iteratively on ESC, including repeated ESC bytes. CAN (`0x18`) and
  SUB (`0x1a`) cancel the current sequence and leave the following key unread.
  Supported restarted arrows remain navigation; unknown restarted CSI is ignored.
- Two new regressions failed before this fix and now pass through both runtime
  readers. They cover CSI/SS3 restarts, repeated ESC, ignored nested CSI, literal
  restarted paste, and CAN/SUB cancellation. Framed-payload tests also cover
  restarted and cancelled tails without crossing packet boundaries.
- Reran formatting, all Rust tests, and the shell suite; all passed. Shell exit
  status was explicitly confirmed as `0`. Kept the `shortcut:` comment convention.
- Live scratch TTY verified restart/cancellation, no leaked search text, normal
  navigation recovery, and restarted literal paste. Disabled terminal ISIG/IXON
  in the scratch harness so CAN/SUB reached the parser rather than terminal
  signal handling. Verified selection rows instead of transient window titles.
  The scratch server was stopped; no production runtime or config changed.
