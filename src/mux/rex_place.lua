-- Rex sidebar layout owner. Rust supplies intent and renders actual PTY sizes;
-- only this script decides whether a divider should move.
local a = rex.args
local function clamp(n, lo, hi) return math.max(lo, math.min(hi, n)) end
local function differs(x, y) return math.abs(x - y) > 0.000001 end
local function seat(node, own)
  if not node then return end
  if node.block_id == own then return { share = node.rect[3] } end
  local split = node.split
  if not split then return end
  for _, before in ipairs({true, false}) do
    local part = before and split.before or split.after
    local other = before and split.after or split.before
    local found = seat(part, own)
    if found then
      if part.block_id == own and split.direction == "horizontal" then
        while other.split do other = before and other.split.before or other.split.after end
        found.border = {
          before = before and own or other.block_id,
          after = before and other.block_id or own,
          sidebar_before = before, span = node.rect[3],
        }
      end
      return found
    end
  end
end

-- Bounded feedback in the sidebar's own font. One liveness probe crosses a
-- minimum-size plateau; a client that sends no fresh size then parks.
local function plan(target, cols, at, previous, now)
  if not at.border or cols == 0 or at.share <= 0 or target == cols then return end
  local lo, hi = at.border.span * 0.05, at.border.span * 0.95
  local p = previous
  if p and (p.target ~= target or (p.parked and cols ~= p.from_columns)) then p = nil end
  local no_response = p and cols == p.from_columns
  if no_response and p.probed then return end
  local lower = p and p.lower or {lo}
  local upper = p and p.upper or {hi}
  if cols < target then lower = {at.share, cols} else upper = {at.share, cols} end
  if upper[1] - lower[1] < 0.000001 then
    if (not differs(at.share, lo) and cols > target)
      or (not differs(at.share, hi) and cols < target) then return end
    lower, upper = {lo}, {hi}
  end
  local share
  if not p then
    share = clamp(at.share * (target + 0.5) / (cols + 0.5), lo, hi)
  elseif no_response then
    if cols < target then
      share = upper[2] and upper[1] or (at.share + upper[1]) / 2
    else
      share = lower[2] and lower[1] or (at.share + lower[1]) / 2
    end
  else
    local fraction = 0.5
    if lower[2] and upper[2] and upper[2] > lower[2] then
      fraction = clamp((target - lower[2]) / (upper[2] - lower[2]), 0.125, 0.875)
    end
    share = lower[1] + (upper[1] - lower[1]) * fraction
  end
  if not differs(share, at.share) then return end
  return { target = target, from_columns = cols, requested_share = share,
    lower = lower, upper = upper, requested = now, probed = not not no_response,
    parked = false, measured_share = no_response and p.measured_share or at.share }
end

local function fit(session, at, share)
  local b = at.border
  if not b then return false end
  local part = clamp(share / b.span, 0.05, 0.95)
  return rex.call("session.resize_pane", {
    session_id = session, direction = "horizontal",
    before_block_id = b.before, after_block_id = b.after,
    ratio = b.sidebar_before and part or 1 - part,
  }) ~= nil
end

local function place(p)
  rex.session.attach{ session_id = p.session }
  local right = p.side == "after"
  local share = tonumber(p.share)
  local ratio = right and 1 - share or share
  local command = {p.exe, "sidebar-pane"}
  if p.client and p.client ~= "" then
    command = {"env", "AGENMUX_REX_CLIENT=" .. p.client, p.exe, "sidebar-pane"}
  end
  local out = assert(rex.call("session.new_split", {
    session_id = p.session, anchor_id = p.anchor, direction = "horizontal",
    side = p.side, ratio = ratio, focus = p.focus,
    layout = {block = {flavor = "com.superlogical.terminal.shell",
      label = p.label, options = {shell = "none", command = command}}},
  }))
  local sidebar, held = out.block_ids[1], 0
  -- Rex's app briefly resets new splits to half. Correct only during creation,
  -- never during daemon handover or when adopting an existing block.
  for _ = 1, 300 do
    local view = rex.call("session.view", {session_id = p.session})
    for _, w in ipairs(view and view.windows or {}) do
      local split = w.layers[1] and w.layers[1].layout.split
      local part = split and (right and split.after or split.before)
      if part and part.block_id == sidebar then
        if math.abs(split.ratio - ratio) > 0.001 then
          held = 0
          rex.call("session.resize_pane", {
            session_id = p.session, direction = "horizontal",
            before_block_id = right and p.edge or sidebar,
            after_block_id = right and sidebar or p.edge, ratio = ratio,
          })
        else held = held + 1 end
      end
    end
    if held >= 60 then break end
    rex.sleep(0.003)
  end
  return sidebar
end

local function window_cells(w, font)
  local widest, result = 0, {0, 0}
  local function rect(node, id)
    if not node then return end
    if node.block_id == id then return node.rect end
    if node.split then return rect(node.split.before, id) or rect(node.split.after, id) end
  end
  for _, b in ipairs(w.blocks or {}) do
    local r, size = rect(w.layout, b.block_id), b.size or {}
    if b.label ~= "agenmux" and b.label ~= "agenmux-popup"
      and r and r[3] > widest and r[4] > 0 then
      widest = r[3]
      local cols = math.floor((size.columns or 0) * math.max(1, size.cell_width_px or 0) / math.max(1, font.cell_width_px or 0))
      local rows = math.floor((size.rows or 0) * math.max(1, size.cell_height_px or 0) / math.max(1, font.cell_height_px or 0))
      result = {math.floor(cols / r[3] + 0.5), math.floor(rows / r[4] + 0.5)}
    end
  end
  return result
end

-- Window lifecycle and placement share this owner with divider feedback.
local function maintain(p, state)
  if not p.exe then return end -- geometry-only caller
  state.had_windows = state.had_windows or {}
  local live = {}
  for _, s in ipairs(p.listing.sessions or {}) do
    live[s.session_id] = true
    rex.session.attach{session_id = s.session_id}
    if #(s.windows or {}) == 0 and state.had_windows[s.session_id] then
      rex.call("session.destroy", {session_id = s.session_id})
    else
      if #(s.windows or {}) > 0 then state.had_windows[s.session_id] = true end
      local empty, real_windows = {}, 0
      for _, w in ipairs(s.windows or {}) do
        local real, sidebar = (w.others or 0) > 0, false
        for _, b in ipairs(w.blocks or {}) do
          sidebar = sidebar or b.label == "agenmux"
          real = real or (b.label ~= "agenmux" and b.label ~= "agenmux-popup")
        end
        if real then
          real_windows = real_windows + 1
          if not sidebar and s.session_id == p.session and s.active_window_id == w.window_id then
            local node = w.layout
            if node and (node.split or node.block_id) then
              local side = p.side or "before"
              local edge = node
              while edge.split do edge = edge.split[side] end
              local dims = window_cells(w, (w.blocks[1] or {}).size or {})
              -- A failed split must not abort reconciliation; the next tick retries.
              pcall(place, {session = s.session_id, anchor = node.split and node.split.split_id or node.block_id,
                edge = edge.block_id, side = side,
                share = dims[1] > 0 and clamp(p.width / dims[1], 0.05, 0.5) or 0.2,
                label = "agenmux", exe = p.exe, client = p.client, focus = false})
            end
          end
        elseif #(w.blocks or {}) > 0 then empty[#empty + 1] = w.window_id end
      end
      if #empty > 0 and real_windows == 0 and #empty == #(s.windows or {}) then
        rex.call("session.destroy", {session_id = s.session_id})
      else
        for _, id in ipairs(empty) do
          rex.call("session.close_window", {session_id = s.session_id, window_id = id})
        end
      end
    end
  end
  for id in pairs(state.had_windows) do if not live[id] then state.had_windows[id] = nil end end
end

local function request(old, target, cols, at, session, now)
  local pending = old.pending
  if cols == target then
    -- Cancelling an unacknowledged request restores the last measured divider.
    local share = pending and pending.from_columns == cols and pending.measured_share or at.share
    if differs(share, at.share) then
      if not fit(session, at, share) then return false end
    end
    old.pending, old.share, old.cols = nil, share, cols
    return true
  end
  local next = plan(target, cols, at, pending, now)
  if next and fit(session, at, next.requested_share) then
    old.pending = next
    return true
  end
  if pending then
    pending.parked, pending.from_columns, pending.requested_share = true, cols, at.share
    pending.measured_share = old.share
  end
  return false
end

local function reconcile(p)
  local state = p.state or {}
  local boot = not state.started
  state.blocks = state.blocks or {}
  local width, now = p.width, p.now
  local changed = state.width and state.width ~= width
  local panes, live, drag = {}, {}, nil
  for _, s in ipairs(p.listing.sessions or {}) do
    rex.session.attach{session_id = s.session_id}
    for _, w in ipairs(s.windows or {}) do
      local kept, duplicate = nil, false
      for _, b in ipairs(w.blocks or {}) do
        if b.label == "agenmux" then
          if kept then
            duplicate = true
            rex.call("block.close", {session_id = s.session_id, block_id = b.block_id})
          else kept = b end
        end
      end
      local b = kept
      if b then
        local at, size = seat(w.layout, b.block_id), b.size or {}
        if at and size.columns and size.columns > 0 and size.rows and size.rows > 0 then
          local id, cols = b.block_id, size.columns
          local dims = window_cells(w, size)
          local count = #(w.blocks or {}) + (w.others or 0)
          local visible = b.owner ~= nil and s.active_window_id == w.window_id
          local sample = table.concat({at.share, cols, size.rows, dims[1], dims[2],
            size.cell_width_px or 0, size.cell_height_px or 0, count}, ":")
          local old = state.blocks[id]
          local fresh = not old
          old = old or {share = at.share, cols = cols, dims = dims, count = count,
            font = {size.cell_width_px or 0, size.cell_height_px or 0}}
          local stable = old.sample == sample
          local reshaped = old.count ~= count
            or old.font[1] ~= (size.cell_width_px or 0) or old.font[2] ~= (size.cell_height_px or 0)
            or math.abs(old.dims[1] - dims[1]) > 2 or math.abs(old.dims[2] - dims[2]) > 2
          local pending = old.pending
          local retry = false
          if pending then
            if cols == pending.target then
              old.pending, old.share, old.cols = nil, at.share, cols
            elseif differs(at.share, pending.requested_share) then
              old.pending = nil -- external layout change supersedes our request
            else
              if cols ~= pending.from_columns then old.share, old.cols = at.share, cols end
              retry = cols ~= pending.from_columns or (not pending.parked and now - pending.requested >= 0.25)
            end
          end
          local dragged = stable and visible and not changed and not reshaped and not duplicate
            and not old.pending and differs(old.share, at.share) and old.cols ~= cols
          if dragged then
            drag, width = id, cols
            old.share, old.cols, old.intent = at.share, cols, nil
          end
          local waiting = not old.pending and differs(old.share, at.share) and old.cols == cols
          if changed or (fresh and not boot) or duplicate or (reshaped and not old.pending and not dragged) then
            old.intent = width
          end
          if visible and stable and not duplicate and not dragged then
            if old.intent and (old.pending or not waiting) then
              local target = clamp(old.intent, 1, math.max(1, dims[1] - 2))
              if request(old, target, cols, at, s.session_id, now) then old.intent = nil end
            elseif old.pending and retry then
              request(old, old.pending.target, cols, at, s.session_id, now)
            end
          end
          if stable and not waiting and not old.pending then
            old.share, old.cols, old.dims, old.count = at.share, cols, dims, count
            old.font = {size.cell_width_px or 0, size.cell_height_px or 0}
          end
          old.sample = sample
          live[id], state.blocks[id] = true, old
          panes[#panes + 1] = {pane = id, win = w.window_id, sess = s.session_id,
            w = cols, h = size.rows, win_size = dims, panes = count,
            active = s.active_window_id == w.window_id, settled = stable and not old.pending,
            width_drag = false}
        end
      end
    end
  end
  local saved
  if drag then
    saved = width
    for id, old in pairs(state.blocks) do if id ~= drag then old.intent = width end end
  end
  for id in pairs(state.blocks) do if not live[id] then state.blocks[id] = nil end end
  maintain(p, state)
  state.started, state.width = true, width
  return {listing = p.listing, state = state, panes = panes, saved_width = saved}
end

-- Both tests and the daemon cross this same interface.
if a.op == "reconcile" then return reconcile(a) end
return place(a)
