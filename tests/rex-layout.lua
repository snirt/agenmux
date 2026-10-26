-- Exercise production reconciliation with a deterministic Rex adapter.
local calls, apply = {}, nil
rex = {session = {attach = function() end}}
rex.call = function(method, args)
  calls[#calls + 1] = {method = method, args = args}
  if apply then apply(method, args) end
  return {}
end
local function fixture(scale, share, minimum, right)
  local cols = math.max(minimum or 1, math.floor(scale * share - 3.2))
  local side = {block_id = "side", rect = {0, 0, share, 1}}
  local shell = {block_id = "shell", rect = {share, 0, 1 - share, 1}}
  local w = {window_id = "w", blocks = {
    {block_id = "side", label = "agenmux", owner = "client", size = {
      columns = cols, rows = 46, cell_width_px = 16, cell_height_px = 32}},
    {block_id = "shell", label = "shell", size = {
      columns = math.floor(scale * (1 - share)), rows = 46, cell_width_px = 16, cell_height_px = 32}},
  }, layout = {rect = {0, 0, 1, 1}, split = {
    direction = "horizontal", ratio = right and 1 - share or share,
    before = right and shell or side, after = right and side or shell}}}
  local p = {op = "reconcile", width = 30, now = 0,
    listing = {sessions = {{session_id = "s", active_window_id = "w", windows = {w}}}}}
  local function change(next_share, update_pty)
    w.layout.split.ratio = right and 1 - next_share or next_share
    side.rect[3], shell.rect[3] = next_share, 1 - next_share
    if update_pty then
      w.blocks[1].size.columns = math.max(minimum or 1, math.floor(scale * next_share - 3.2))
      w.blocks[2].size.columns = math.floor(scale * (1 - next_share))
    end
  end
  return p, w, change
end
local function tick(p)
  p.now = p.now + 0.3
  rex.args = p
  local result = dofile("src/mux/rex_place.lua")
  p.state = result.state
  if result.saved_width then p.width = result.saved_width end
  return result
end
local function reset() calls, apply = {}, nil end

-- Handover adopts existing geometry, even if it differs from configured width.
for _, right in ipairs({false, true}) do
  reset()
  local p, w = fixture(174, 0.27, 16, right)
  local ratio, id = w.layout.split.ratio, w.blocks[1].block_id
  for _ = 1, 20 do tick(p) end
  assert(#calls == 0 and w.layout.split.ratio == ratio and w.blocks[1].block_id == id,
    "adoption or repeated events must not move a split")
  p.state = nil
  for _ = 1, 20 do tick(p) end
  assert(#calls == 0, "fresh daemon must preserve dragged geometry")
end
print("ok   rex-lua-handover-and-idle-no-resizes")

-- PTY-late divider changes are never mistaken for settled geometry.
reset()
local p, w, change = fixture(174, 0.2, 16)
tick(p); tick(p)
change(0.25, false)
for _ = 1, 5 do assert(not tick(p).saved_width) end
assert(#calls == 0)
change(0.25, true)
assert(not tick(p).saved_width)
assert(tick(p).saved_width == w.blocks[1].size.columns)
assert(#calls == 0, "dragged pane must never be resized")
p.state = nil
for _ = 1, 5 do tick(p) end
assert(#calls == 0, "drag width must survive reload")
print("ok   rex-lua-delayed-drag-and-reload")

-- Feedback uses real PTY columns; both docking sides and minimum plateaus.
for _, right in ipairs({false, true}) do
  for _, scale in ipairs({100, 174, 240}) do
    for _, start in ipairs({0.05, 0.08, 0.2, 0.7}) do
      for _, target in ipairs({17, 18, 30, 60}) do
        reset()
        local p, w, change = fixture(scale, start, 16, right)
        p.width = w.blocks[1].size.columns
        tick(p); tick(p)
        apply = function(method, args)
          assert(method == "session.resize_pane")
          assert(args.before_block_id == (right and "shell" or "side"))
          change(right and 1 - args.ratio or args.ratio, true)
        end
        p.width = target
        for _ = 1, 80 do tick(p) end
        assert(w.blocks[1].size.columns == target,
          string.format("scale=%s start=%s target=%s actual=%s", scale, start, target, w.blocks[1].size.columns))
        local count = #calls
        for _ = 1, 10 do tick(p) end
        assert(#calls == count, "settled layout must not churn")
      end
    end
  end
end
print("ok   rex-lua-feedback-fonts-plateaus-and-right-docking")

reset()
p, w = fixture(174, 0.2, 16)
tick(p); tick(p)
p.width = 17
apply = function(_, args)
  -- Layout accepts the request, but hidden/unresponsive client sends no PTY size.
  local split = w.layout.split
  split.ratio, split.before.rect[3], split.after.rect[3] = args.ratio, args.ratio, 1 - args.ratio
end
for _ = 1, 20 do tick(p) end
assert(#calls <= 2, "unresponsive client must park after one probe")
print("ok   rex-lua-unresponsive-client-bounded")

reset()
p, w = fixture(174, 0.2, 16)
tick(p); tick(p)
w.blocks[1].owner = nil
p.width = 60
for _ = 1, 10 do tick(p) end
assert(#calls == 0, "unowned default geometry cannot drive a resize")
print("ok   rex-lua-unowned-geometry-not-authority")

-- A constrained step acknowledges its actual divider; a later drag stays a drag.
reset()
p, w, change = fixture(174, 0.2, 16)
tick(p); tick(p)
apply = function(_, args) change(args.ratio, true) end
p.width = 1
for _ = 1, 20 do tick(p) end
assert(w.blocks[1].size.columns == 16 and #calls <= 2)
calls = {}
change(0.2, true)
tick(p)
assert(tick(p).saved_width == 31)
assert(#calls == 0)
print("ok   rex-lua-constrained-step-then-drag")

-- Font changes alone never publish a drag; frames keep using actual PTY sizes.
reset()
p, w, change = fixture(174, 0.2, 16)
p.width = w.blocks[1].size.columns
tick(p); tick(p)
w.blocks[1].size.cell_width_px = 20
w.blocks[1].size.columns = 24
local result = tick(p)
assert(not result.saved_width and result.panes[1].w == 24)
result = tick(p)
assert(not result.saved_width)
assert(#calls == 1 and calls[1].method == "session.resize_pane")
print("ok   rex-lua-font-resize-not-drag")

-- Desired width can cancel an outstanding request before the PTY responds.
reset()
p, w, change = fixture(174, 0.2, 16)
local initial, cols = w.layout.split.ratio, w.blocks[1].size.columns
p.width = cols
tick(p); tick(p)
apply = function(_, args) change(args.ratio, false) end
p.width = 17
tick(p)
p.width = cols
tick(p); tick(p)
assert(math.abs(w.layout.split.ratio - initial) < 0.000001)
assert(w.blocks[1].size.columns == cols)
print("ok   rex-lua-cancel-pending-without-stale-ratio")

-- Nested splits resize the adjacent border in its parent's coordinate space.
reset()
p, w = fixture(174, 0.2, 16)
local side = w.layout.split.before
local neighbor = w.layout.split.after
neighbor.rect = {0.2, 0, 0.2, 0.5}
w.blocks[2].size.columns = 34
w.layout = {rect = {0, 0, 1, 1}, split = {
  direction = "horizontal", ratio = 0.4,
  before = {rect = {0, 0, 0.4, 1}, split = {
    direction = "horizontal", ratio = 0.5, before = side,
    after = {rect = {0.2, 0, 0.2, 1}, split = {
      direction = "vertical", ratio = 0.5, before = neighbor,
      after = {block_id = "lower", rect = {0.2, 0.5, 0.2, 0.5}}}}}},
  after = {block_id = "outer", rect = {0.4, 0, 0.6, 1}}}}
p.width = w.blocks[1].size.columns
tick(p); tick(p)
p.width = 45
tick(p)
assert(#calls == 1 and calls[1].method == "session.resize_pane")
assert(calls[1].args.before_block_id == "side" and calls[1].args.after_block_id == "shell")
local expected = 0.2 * (45 + 0.5) / (31 + 0.5) / 0.4
assert(math.abs(calls[1].args.ratio - expected) < 0.000001,
  "nested resize must use the adjacent split's span, not the whole window")
print("ok   rex-lua-nested-adjacent-border")

-- Lua owns drag propagation too; Rust need not infer or resize peer dividers.
reset()
p, w, change = fixture(174, 0.2, 16)
local peer, peer_window = fixture(174, 0.2, 16)
local peer_session = peer.listing.sessions[1]
peer_session.session_id, peer_session.active_window_id = "peer", "peer-window"
peer_window.window_id = "peer-window"
peer_window.blocks[1].block_id, peer_window.layout.split.before.block_id = "peer-side", "peer-side"
peer_window.blocks[2].block_id, peer_window.layout.split.after.block_id = "peer-shell", "peer-shell"
p.listing.sessions[2] = peer_session
p.width = w.blocks[1].size.columns
tick(p); tick(p)
change(0.25, true)
tick(p)
assert(tick(p).saved_width == w.blocks[1].size.columns)
assert(#calls == 0, "drag adoption must not move the dragged divider")
tick(p)
assert(#calls == 1 and calls[1].method == "session.resize_pane"
  and calls[1].args.session_id == "peer", "Lua must propagate drag intent to the peer")
print("ok   rex-lua-drag-propagates-to-peer")

-- Lifecycle: only an observed empty session is destroyed; real/nonterminal
-- windows and sessions still being created survive.
reset()
p = {op = "reconcile", width = 30, now = 0, exe = "/bin/agenmux", listing = {sessions = {
  {session_id = "new", windows = {}},
  {session_id = "observed", windows = {{window_id = "real", blocks = {}, others = 1}}},
}}}
tick(p)
assert(#calls == 0)
p.listing.sessions[2].windows = {}
tick(p)
assert(#calls == 1 and calls[1].method == "session.destroy"
  and calls[1].args.session_id == "observed")
print("ok   rex-lua-window-lifecycle")

-- Close sidebar-only windows without destroying windows with real content.
reset()
p, w = fixture(174, 0.2, 16)
p.exe = "/bin/agenmux"
p.listing.sessions[1].windows[2] = {window_id = "orphan", blocks = {
  {block_id = "orphan-side", label = "agenmux"}}}
p.listing.sessions[1].windows[3] = {window_id = "nonterminal", blocks = {}, others = 1}
tick(p)
assert(#calls == 1 and calls[1].method == "session.close_window"
  and calls[1].args.window_id == "orphan")

reset()
p, w = fixture(174, 0.2, 16)
p.exe = "/bin/agenmux"
w.blocks[2] = nil
w.layout = w.layout.split.before
tick(p)
assert(#calls == 1 and calls[1].method == "session.destroy"
  and calls[1].args.session_id == "s")
print("ok   rex-lua-sidebar-only-window-and-session-cleanup")
