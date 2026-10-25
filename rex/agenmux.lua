-- agenmux for Rex. Load it from ~/.config/rex/init.lua:
--
--   dofile("/path/to/agenmux/rex/agenmux.lua"){ bin = "/path/to/agenmux" }
--
-- Options:
--   bin        the agenmux binary (default: "agenmux" on the server's PATH)
--   key        toggles the sidebar in the current window (default "alt+s")
--   popup_key  shows the sidebar as a floating popup (default "alt+e";
--              false binds nothing)
--
-- For a sidebar in every window, as the tmux integration places them, run
-- `agenmux rex-sidebars` once per Rex server start.
return function(opts)
  opts = opts or {}
  local bin = opts.bin or "agenmux"

  local function toggle(mode)
    return function(ctx)
      -- detached: placing the sidebar outlives this action
      assert(rex.exec.start({ bin, "rex-toggle", ctx.session_id, ctx.block_id or "", mode },
        { detach = true }))
    end
  end

  rex.action{
    name = "agenmux_sidebar",
    title = "Toggle agenmux sidebar",
    run = toggle("split"),
  }
  rex.action{
    name = "agenmux_popup",
    title = "Show agenmux popup",
    run = toggle("popup"),
  }
  rex.bind(opts.key or "alt+s", "agenmux_sidebar")
  if opts.popup_key ~= false then
    rex.bind(opts.popup_key or "alt+e", "agenmux_popup")
  end
end
