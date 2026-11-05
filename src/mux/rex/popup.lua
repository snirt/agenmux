-- Decide what the popup key does over `block`, in one server round trip, and
-- size a new popup: the configured cells as a share of the block's window.
-- Rex sizes layers by share, so the cells go through the block's own size
-- and its share of the window. The caller creates or focuses the layer
-- itself: done from this attached script, the app never draws it.
local a = rex.args
-- calls into a session need the script attached to it
rex.session.attach{ session_id = a.session }
local view = assert(rex.call("session.view", { session_id = a.session }))
local function rect_of(node)
  if node.block_id == a.block then
    return node.rect
  end
  local split = node.split
  return split and (rect_of(split.before) or rect_of(split.after))
end
local window, rect
for _, w in ipairs(view.windows or {}) do
  for _, layer in ipairs(w.layers or {}) do
    for _, b in ipairs(layer.blocks or {}) do
      if b.block_id == a.block then
        window = w
        rect = layer.kind == "tiled" and rect_of(layer.layout) or nil
      end
    end
  end
end
assert(window, "no window holds " .. a.block)
-- One popup per window, and the key toggles it: pressed in the popup it
-- closes it. Pressed elsewhere it focuses it: the app draws a floating
-- layer only while it holds focus, so an unfocused one is out of sight.
for _, layer in ipairs(window.layers or {}) do
  for _, b in ipairs(layer.blocks or {}) do
    if b.label == a.label then
      if window.focused_block_id == b.block_id then
        rex.call("block.close", { session_id = a.session, block_id = b.block_id })
        return { action = "closed" }
      end
      return { action = "focus", window = window.window_id, block = b.block_id }
    end
  end
end
local bounds = { 0.2, 0.1, 0.6, 0.8 }
if rect and rect[3] > 0 and rect[4] > 0 then
  local size = rex.block.call("com.superlogical.terminal", "size",
    { session_id = a.session, block_id = a.block })
  if size and size.columns and size.rows then
    local columns, rows = size.columns / rect[3], size.rows / rect[4]
    local w = math.min(math.max(a.width / columns, 0.1), 1)
    -- height 0: auto
    local h = a.height > 0 and math.min(math.max(a.height / rows, 0.1), 1) or 0.8
    bounds = { (1 - w) / 2, (1 - h) / 2, w, h }
  end
end
return { action = "open", window = window.window_id, bounds = bounds }
