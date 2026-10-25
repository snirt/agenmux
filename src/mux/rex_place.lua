-- Place the sidebar split at `ratio` and keep it there. The app resets a new
-- split in a window it shows to half, a few milliseconds after the server
-- applied the ratio; correcting it from here, with no CLI round trips,
-- lands before the app draws the half width.
local a = rex.args
-- `ratio` is the sidebar's share; a split's ratio is its before part's
local right = a.side == "after"
local share = right and 1 - a.ratio or a.ratio
local command = { a.exe, "sidebar" }
if a.client and a.client ~= "" then
  command = { "env", "AGENMUX_REX_CLIENT=" .. a.client, a.exe, "sidebar" }
end
-- calls into a session need the script attached to it
rex.session.attach{ session_id = a.session }
local out = assert(rex.call("session.new_split", {
  session_id = a.session,
  anchor_id = a.anchor,
  direction = "horizontal",
  side = a.side,
  ratio = share,
  focus = a.focus, -- rex do parses true/false
  layout = { block = {
    flavor = "com.superlogical.terminal.shell",
    label = a.label,
    options = { shell = "none", command = command },
  } },
}))
local sidebar = out.block_ids[1]
local held = 0
-- ~1s at most; done once the border has held for ~200ms
for _ = 1, 300 do
  local view = rex.call("session.view", { session_id = a.session })
  for _, w in ipairs(view and view.windows or {}) do
    local split = w.layers[1] and w.layers[1].layout.split
    local part = split and (right and split.after or split.before)
    if part and part.block_id == sidebar then
      if math.abs(split.ratio - share) > 0.001 then
        held = 0
        rex.call("session.resize_pane", {
          session_id = a.session,
          direction = "horizontal",
          before_block_id = right and a.edge or sidebar,
          after_block_id = right and sidebar or a.edge,
          ratio = share,
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
