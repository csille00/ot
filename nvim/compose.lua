-- A reply to a thread, written in your own nvim in a box over ot. :w sends it,
-- :q keeps it for next time.
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
vim.wo.winbar = "%#Comment#" .. hint
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
  local win = api.nvim_get_current_win()
  local row = api.nvim_win_get_cursor(win)[1]
  local here = api.nvim_buf_get_lines(draft, row - 1, row, false)[1] or ""
  local from = vim.trim(here) == "" and row - 1 or row
  api.nvim_buf_set_lines(draft, from, row, false, { ("![%s](%s)"):format(name, path) })
  vim.notify("attached " .. name)
end))

if vim.trim(table.concat(lines(), "\n")) == "" then
  vim.cmd("startinsert")
else
  api.nvim_win_set_cursor(0, { api.nvim_buf_line_count(draft), 0 })
end
