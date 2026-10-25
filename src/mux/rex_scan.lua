-- One `rex do` per scan: every session's windows and terminal blocks, with
-- each block's process, title and size, instead of three CLI forks per block.
local terminal = "com.superlogical.terminal"
local sessions = {}
for _, s in ipairs(assert(rex.call("session.list")).sessions or {}) do
  -- calls into a session need the script attached to it
  rex.session.attach{ session_id = s.session_id }
  local view = rex.call("session.view", { session_id = s.session_id })
  if view then
    local windows = {}
    for _, w in ipairs(view.windows or {}) do
      local blocks, layout, others = {}, nil, 0
      for _, layer in ipairs(w.layers or {}) do
        if layer.kind == "tiled" then layout = layer.layout end
        for _, b in ipairs(layer.blocks or {}) do
          if b.creator_name == terminal then
            local q = { session_id = s.session_id, block_id = b.block_id }
            blocks[#blocks + 1] = {
              block_id = b.block_id,
              label = b.label,
              process = rex.block.call(terminal, "process", q),
              title = rex.block.call(terminal, "title", q),
              size = rex.block.call(terminal, "size", q),
            }
          else
            others = others + 1
          end
        end
      end
      windows[#windows + 1] = {
        window_id = w.window_id,
        label = w.label,
        focused_block_id = w.focused_block_id,
        layout = layout,
        -- non-terminal blocks: a window holding one is not sidebar-only
        others = others,
        blocks = blocks,
      }
    end
    sessions[#sessions + 1] = {
      session_id = s.session_id,
      label = s.label,
      active_window_id = view.active_window_id,
      windows = windows,
    }
  end
end
return { sessions = sessions }
