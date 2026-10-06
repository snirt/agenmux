-- Git integration: a branch badge on every pane inside a repository, a commit
-- list (gc) that opens `git show` in a new window, and a branch switcher (gb).
--
-- One `git status` per repository at most every few seconds: results are keyed
-- by cwd and cached, so many panes in the same repository share one process.

local STATUS_TTL_MS = 3000

local function lines(text)
  local out = {}
  for line in text:gmatch("[^\n]+") do
    out[#out + 1] = line
  end
  return out
end

-- fresh skips the shared cache, for right after this plugin changed the repo.
local function refresh(pane, fresh)
  if not pane.cwd or pane.cwd == "" then
    return
  end
  local opts = { cwd = pane.cwd, timeout_ms = 1000 }
  if not fresh then
    opts.key = "git-status:" .. pane.cwd
    opts.ttl_ms = STATUS_TTL_MS
  end
  agenmux.system(
    { "git", "status", "--porcelain=v1", "--branch", "--untracked-files=no" },
    opts,
    function(r)
      if r.code ~= 0 then
        agenmux.ui.badge(pane.id, "git", nil)
        return
      end
      local out = lines(r.stdout)
      local head = (out[1] or ""):gsub("^## ", "")
      local branch = head:match("^No commits yet on (%S+)") or head:match("^([^%.%s]+)") or "?"
      if head:match("^HEAD %(no branch%)") then
        branch = "detached"
      end
      local dirty = #out > 1
      local ahead = head:match("ahead (%d+)")
      local text = "\u{e0a0}" .. branch .. (dirty and "*" or "") .. (ahead and ("↑" .. ahead) or "")
      agenmux.ui.badge(pane.id, "git", text, dirty and "warn" or "ok")
    end
  )
end

agenmux.on("PaneAdded", function(ev)
  refresh(ev.pane)
end)

agenmux.on("SelectionChanged", function(ev)
  refresh(ev.pane)
end)

-- An agent that finished a turn has probably edited files.
agenmux.on("AgentStateChanged", function(ev)
  if ev.new == "idle" or ev.new == "done" then
    refresh(ev.pane)
  end
end)

agenmux.keymap.set("gc", function(pane)
  if not pane then
    return
  end
  agenmux.system(
    { "git", "log", "--oneline", "--no-decorate", "-n", "50" },
    { cwd = pane.cwd },
    function(r)
      if r.code ~= 0 then
        agenmux.notify("not a git repository")
        return
      end
      agenmux.ui.list({
        title = "commits",
        items = lines(r.stdout),
        on_select = function(item)
          local sha = item:match("^(%x+)")
          -- git's default `less -F` would close a one-screen commit at once.
          agenmux.api.open_window({
            pane = pane.id,
            cmd = { "git", "-c", "core.pager=less -+F -R", "show", sha },
          })
        end,
      })
    end
  )
end, { desc = "git log" })

agenmux.keymap.set("gb", function(pane)
  if not pane then
    return
  end
  agenmux.system(
    { "git", "branch", "--format=%(HEAD) %(refname:short)" },
    { cwd = pane.cwd },
    function(r)
      if r.code ~= 0 then
        agenmux.notify("not a git repository")
        return
      end
      agenmux.ui.list({
        title = "branches",
        items = lines(r.stdout),
        on_select = function(item)
          local branch = item:sub(3)
          agenmux.system({ "git", "switch", branch }, { cwd = pane.cwd }, function(s)
            local message = s.code == 0 and ("switched to " .. branch)
              or (s.stderr:match("[^\n]+") or "git switch failed")
            agenmux.notify(message)
            refresh(pane, true)
          end)
        end,
      })
    end
  )
end, { desc = "git branches" })
