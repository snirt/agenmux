-- agenmux for Rex. Load it from ~/.config/rex/init.lua:
--
--   dofile("/path/to/agenmux/rex/agenmux.lua"){ bin = "/path/to/agenmux" }
--
-- Options:
--   bin        the agenmux binary (default: "agenmux" on the server's PATH)
--   key        toggles the sidebar in the current window (default "alt+s")
--   popup_key  opens or closes the sidebar as a floating popup (default "alt+p";
--              false binds nothing). Not alt+e: on a US Mac keyboard Option+E
--              is a dead key, which never reaches Rex
--
-- For a sidebar in every window, as the tmux integration places them, run
-- `agenmux rex-sidebars` once per Rex server start.
return function(opts)
  opts = opts or {}
  local bin = opts.bin or "agenmux"

  local function toggle(mode)
    return function(ctx)
      -- detached: placing the sidebar outlives this action. The client is
      -- the app window the key came from: the sidebar navigates that one.
      local args = { bin, "rex-toggle", ctx.session_id, ctx.block_id or "", mode, ctx.client_id or "" }
      assert(rex.exec.start(args, { detach = true }))
    end
  end

  rex.action{
    name = "agenmux_sidebar",
    title = "Toggle agenmux sidebar",
    run = toggle("split"),
  }
  rex.action{
    name = "agenmux_popup",
    title = "Toggle agenmux popup",
    run = toggle("popup"),
  }
  rex.action{
    name = "agenmux_reload",
    title = "Reload agenmux without changing layout",
    run = function(ctx)
      assert(rex.exec.start({bin, "rex-reload", ctx.client_id or ""}, {detach = true}))
    end,
  }
  rex.bind(opts.key or "alt+s", "agenmux_sidebar")
  if opts.popup_key ~= false then
    rex.bind(opts.popup_key or "alt+p", "agenmux_popup")
  end
end
