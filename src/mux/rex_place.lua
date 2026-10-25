-- Place the sidebar split at `ratio` and keep it there. The app resets a new
-- split in a window it shows to half, a few milliseconds after the server
-- applied the ratio; correcting it from here, with no CLI round trips,
-- lands before the app draws the half width.
local a = rex.args
-- calls into a session need the script attached to it
rex.session.attach{ session_id = a.session }
local out = assert(rex.call("session.new_split", {
  session_id = a.session,
  anchor_id = a.anchor,
  direction = "horizontal",
  side = "before",
  ratio = a.ratio,
  focus = a.focus, -- rex do parses true/false
  layout = { block = {
    flavor = "com.superlogical.terminal.shell",
    label = a.label,
    options = { shell = "none", command = { a.exe, "sidebar" } },
  } },
}))
local sidebar = out.block_ids[1]
local held = 0
-- ~1s at most; done once the border has held for ~200ms
for _ = 1, 300 do
  local view = rex.call("session.view", { session_id = a.session })
  for _, w in ipairs(view and view.windows or {}) do
    local split = w.layers[1] and w.layers[1].layout.split
    if split and split.before.block_id == sidebar then
      if math.abs(split.ratio - a.ratio) > 0.001 then
        held = 0
        rex.call("session.resize_pane", {
          session_id = a.session,
          direction = "horizontal",
          before_block_id = sidebar,
          after_block_id = a.right,
          ratio = a.ratio,
        })
      else
        held = held + 1
      end
    end
  end
  if held >= 60 then
    break
  end
  rex.sleep(0.003)
end
return sidebar
