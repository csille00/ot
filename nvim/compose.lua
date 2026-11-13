-- A reply to a thread, written in your own nvim: the draft in a float over the
-- conversation, or alone in a zellij pane. :w sends it, :q keeps it for next time.
local api = vim.api
local uv = vim.uv or vim.loop
local meta = vim.json.decode(table.concat(vim.fn.readfile(vim.g.ot_compose), "\n"))
local draft = api.nvim_get_current_buf()
vim.bo[draft].filetype = "markdown"
vim.bo[draft].bufhidden = "hide"
vim.bo[draft].swapfile = false

local sent = false
local function lines()
  return api.nvim_buf_get_lines(draft, 0, -1, false)
end
-- Whatever way nvim goes, an unsent draft is kept for the next s.
local function keep()
  if not sent and api.nvim_buf_is_valid(draft) then
    vim.fn.writefile(lines(), meta.draft)
    vim.bo[draft].modified = false
  end
end

api.nvim_create_autocmd("BufWritePost", {
  buffer = draft,
  callback = function()
    if vim.trim(table.concat(lines(), "\n")) == "" then
      vim.notify("nothing to send", vim.log.levels.WARN)
      return
    end
    sent = true
    vim.fn.writefile({}, meta.sent)
    vim.schedule(function()
      vim.cmd("qa!")
    end)
  end,
})
api.nvim_create_autocmd("QuitPre", { buffer = draft, callback = keep })
api.nvim_create_autocmd("VimLeavePre", { callback = keep })

local hint = " :w sends · :q keeps the draft · copy an image on the Mac and send it here to attach it "
local float
if meta.plain then
  vim.wo.winbar = "%#Title# " .. meta.title .. "%#Comment#  ·" .. hint
else
  -- The conversation behind the draft, its end in view.
  local bg = api.nvim_create_buf(false, true)
  api.nvim_buf_set_lines(bg, 0, -1, false, vim.fn.readfile(meta.context))
  vim.bo[bg].filetype = "markdown"
  vim.bo[bg].modifiable = false
  vim.bo[bg].bufhidden = "wipe"
  api.nvim_win_set_buf(0, bg)
  api.nvim_win_set_cursor(0, { math.max(1, api.nvim_buf_line_count(bg)), 0 })
  local w = math.min(100, vim.o.columns - 8)
  local h = math.max(8, math.floor(vim.o.lines * 0.45))
  float = api.nvim_open_win(draft, true, {
    relative = "editor",
    width = w,
    height = h,
    row = math.floor((vim.o.lines - h) / 2) - 1,
    col = math.floor((vim.o.columns - w) / 2),
    border = "rounded",
    title = " " .. meta.title .. " ",
    title_pos = "center",
    footer = hint,
    footer_pos = "center",
  })
  vim.wo[float].wrap, vim.wo[float].linebreak = true, true
  -- Closing the draft's window ends the reply, kept, as :q does.
  api.nvim_create_autocmd("WinClosed", {
    pattern = tostring(float),
    callback = function()
      keep()
      vim.schedule(function()
        vim.cmd("qa!")
      end)
    end,
  })
end
vim.wo.wrap, vim.wo.linebreak = true, true

-- An image the Mac sends lands in the paste folder; it goes in under the cursor.
vim.fn.mkdir(meta.paste, "p")
local seen = {}
for _, f in ipairs(vim.fn.readdir(meta.paste)) do
  seen[f] = true
end
local watch = uv.new_fs_event()
watch:start(meta.paste, {}, vim.schedule_wrap(function(_, name)
  if not name or seen[name] or name:sub(1, 1) == "." then
    return
  end
  local path = meta.paste .. "/" .. name
  if vim.fn.filereadable(path) == 0 then
    return
  end
  seen[name] = true
  local win = float and api.nvim_win_is_valid(float) and float or api.nvim_get_current_win()
  local row = api.nvim_win_get_cursor(win)[1]
  local here = api.nvim_buf_get_lines(draft, row - 1, row, false)[1] or ""
  local from = vim.trim(here) == "" and row - 1 or row
  api.nvim_buf_set_lines(draft, from, row, false, { ("![%s](%s)"):format(name, path) })
  vim.notify("attached " .. name)
end))

if vim.trim(table.concat(lines(), "\n")) == "" then
  vim.cmd("startinsert")
else
  api.nvim_win_set_cursor(float or 0, { api.nvim_buf_line_count(draft), 0 })
end
