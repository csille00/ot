-- The plugin against what src/nvim.rs hands it, run by that file's test in a
-- headless nvim without anyone's config. Fails with each broken behavior.
local api = vim.api
local failed = {}
local function check(ok, what)
  if not ok then
    failed[#failed + 1] = what
  end
end
local function list()
  return api.nvim_buf_get_lines(0, 0, -1, false)
end
local function has(lines, pattern)
  for _, l in ipairs(lines) do
    if l:find(pattern) then
      return true
    end
  end
  return false
end
local function press(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), "mx", false)
end
local function code()
  for _, w in ipairs(api.nvim_list_wins()) do
    if w ~= api.nvim_get_current_win() then
      return w
    end
  end
end
local function code_line()
  return api.nvim_win_get_cursor(code())[1]
end

-- A file's first visit loads it in the code window, so your plugins'
-- autocommands run there, not in the float nvim would flash up for them.
local in_float = 0
api.nvim_create_autocmd({ "BufReadPost", "FileType" }, {
  callback = function(a)
    local review_file = a.file:find("src/sum") ~= nil
    in_float = in_float + ((review_file and api.nvim_win_get_config(0).relative ~= "") and 1 or 0)
  end,
})

-- What a markdown renderer hears of the change redrawn while you are in
-- the list, which nvim alone never tells it.
local told = 0
api.nvim_create_autocmd("TextChanged", {
  callback = function(a)
    if a.buf ~= api.nvim_get_current_buf() and api.nvim_buf_get_name(a.buf):find("the change$") then
      told = told + 1
    end
  end,
})

-- Whatever stalls, the run ends: a hung test is worse than a failed one.
vim.defer_fn(function()
  io.stderr:write("the plugin test took too long\n")
  vim.cmd("cquit 3")
end, 20000)

vim.wait(3000, function()
  return has(list(), "The change") and has(list(), "report 2")
end, 10)

local l = list()
check(has(l, "^!301  Sum with reduce"), "the title heads the list")
check(has(l, "needs approval"), "the header says what the MR needs before it can merge")
check(has(l, "^▸ · report 2 +1 +%+1 +−1 *$"), "report closed over its two, with its one open thread and its counts in columns")
check(has(l, "^  − unused +−2 *$"), "unused is a root, removed, with nothing under it")
check(not has(l, "total"), "everything starts closed: total stays under report")
check(has(l, "unused"), "unused is a root")
check(has(l, "sum.ts .*✓$"), "the import's change was viewed before")
check(has(l, "1 TEST FILE   t shows them"), "the test file is set aside")
check(has(l, "^♪ A song") and has(l, "paused"), "the list says what cliamp plays, and that it is paused")

-- M opens the player over the review, with the whole of it; M again closes it.
press("M")
local float
for _, w in ipairs(api.nvim_list_wins()) do
  if api.nvim_win_get_config(w).relative ~= "" then
    float = w
  end
end
check(float ~= nil, "M opens the player")
check(float and has(api.nvim_buf_get_lines(api.nvim_win_get_buf(float), 0, -1, false), "^ 1:40 "), "which shows the clock")
check(not has(list(), "^♪"), "and the list's music line steps aside for it")
press("M")
check(#api.nvim_list_wins() == 2, "M again closes the player")
check(has(list(), "^♪ A song"), "and the line is back")

-- Narrowed below its widest row, the list widens back: names stay whole.
local list_win = api.nvim_get_current_win()
api.nvim_win_set_width(list_win, 12)
vim.cmd.doautocmd("VimResized")
check(has(list(), "^▸ · report 2 +1 +%+1 +−1 *$"), "a narrowed list keeps report whole on one line")
check(has(list(), "^  · …/sum%.ts .*✓$"), "but cuts the import's file to its name rather than widen for its folder")
check(api.nvim_win_get_width(list_win) == 25, "and is just wide enough for report, got " .. api.nvim_win_get_width(list_win))
api.nvim_win_set_width(list_win, 30)
vim.cmd.doautocmd("VimResized")
-- The review opens on what the MR says it does.
check(api.nvim_buf_get_name(api.nvim_win_get_buf(code())):find("the change$") ~= nil, "it opens on the change")
check(has(api.nvim_buf_get_lines(api.nvim_win_get_buf(code()), 0, -1, false), "^Sums with reduce%.$"), "which shows the description")
-- Its picture and video as ot's copies, drawn again once they land while
-- the list has the cursor, and the renderer told.
local change = api.nvim_win_get_buf(code())
vim.wait(2000, function()
  return has(api.nvim_buf_get_lines(change, 0, -1, false), "demo%.mp4%.png")
end, 10)
local said = api.nvim_buf_get_lines(change, 0, -1, false)
check(has(said, "^!%[shot%]%(/got/ab12/shot%.png%)$"), "the picture is ot's copy, without GitLab's size")
check(has(said, "^%[▶ demo%]%(/got/cd34/demo%.mp4%) !%[%]%(/got/cd34/demo%.mp4%.png%)$"), "the video is its first frame under a link that plays it")
check(told > 0, "the renderer hears the change redrawn from the list")
-- Left and come back to, the change keeps its lines, so what a plugin
-- hangs on one (a picture) stays on it.
local row
for i, l in ipairs(said) do
  row = l == "Sums with reduce." and i - 1 or row
end
local hung = api.nvim_buf_set_extmark(change, api.nvim_create_namespace("test.change"), row, 0, {})
press("j")
press("k")
check(api.nvim_buf_get_extmark_by_id(change, api.nvim_create_namespace("test.change"), hung, {})[1] == row, "a mark on the change stays on its line after you leave and come back")
press("j")
check(api.nvim_buf_get_name(api.nvim_win_get_buf(code())):find("src/sum.ts$") ~= nil, "the code shows report's file")
check(code_line() == 7, "the code shows report from its first line, got " .. code_line())

-- y copies report as you'd name it, here to a clipboard that only listens.
local copied
local function heard(lines)
  copied = lines
end
vim.g.clipboard = { name = "test", copy = { ["+"] = heard, ["*"] = heard }, paste = { ["+"] = function() return {} end, ["*"] = function() return {} end } }
press("y")
check(vim.deep_equal(copied, { "report in src/sum.ts" }), "y copies report in src/sum.ts, got " .. vim.inspect(copied))

local underlined = 0
for _, m in ipairs(api.nvim_buf_get_extmarks(api.nvim_win_get_buf(code()), -1, { 7, 0 }, { 7, -1 }, { details = true })) do
  underlined = underlined + (m[4].hl_group == "OtUses" and 1 or 0)
end
check(underlined == 3, "report's line 8 underlines total twice and count once, got " .. underlined)

-- The rest of report's file fades: total's lines and its removed ones, but
-- not report's own, even the removed ones drawn down at count's line 11.
local function faded(line)
  local buf = api.nvim_win_get_buf(code())
  for _, m in ipairs(api.nvim_buf_get_extmarks(buf, -1, { line - 1, 0 }, { line - 1, -1 }, { details = true, overlap = true })) do
    if m[4].hl_group == "OtFaint" and m[2] < line and m[4].end_row >= line then
      return true
    end
  end
  return false
end
local function removed_hl(text)
  for _, m in ipairs(api.nvim_buf_get_extmarks(api.nvim_win_get_buf(code()), -1, 0, -1, { details = true })) do
    for _, vl in ipairs(m[4].virt_lines or {}) do
      local whole = ""
      for _, chunk in ipairs(vl) do
        whole = whole .. chunk[1]
      end
      if whole:find(text, 1, true) then
        return vl[1][2]
      end
    end
  end
end
check(faded(3) and faded(11) and not faded(7) and not faded(9), "on report, total and count fade and report doesn't")
check(vim.deep_equal(removed_hl("let sum = 0"), { "OtDel", "OtFaint" }), "total's removed lines fade too")
check(removed_hl('return "total "') == "OtDel", "report's removed lines stay lit, got " .. vim.inspect(removed_hl('return "total "')))

press("]")
check(code_line() == 8, "the first ] goes to report's first hunk, got " .. code_line())
press("]")
check(code_line() == 11, "] goes to report's next hunk, got " .. code_line())
check(faded(3), "report's own hunk at count's line keeps the fade")
press("]")
check(code_line() == 11, "] stays at report's last hunk")
press("[")
press("[")
check(code_line() == 8, "[ stops at report's first hunk, got " .. code_line())

-- Leave report on its second hunk; coming back shows report whole again.
press("]")
press("]")
press("j")
check(code_line() ~= 11, "unused shows its own code")
press("k")
check(code_line() == 7, "back on report, the code shows report from its first line, got " .. code_line())
press("]")
check(code_line() == 8, "and ] starts over at report's first hunk")

press("l")
l = list()
check(has(l, "^├─   · total"), "l opens report to what it uses, rails and all")
check(has(l, "count"), "count is under report too")
press("j")
check(api.nvim_get_current_line():find("total") ~= nil, "j moves to total")
check(code_line() == 3, "the code shows total from its first line, got " .. code_line())
press("h")
check(api.nvim_get_current_line():find("report") ~= nil and not has(list(), "total"), "h from under report closes it")

press("?")
check(has(list(), "^%] +next hunk"), "? shows the keys in the list's place")
press("?")
check(has(list(), "The change"), "? again brings the list back")

check(vim.o.guicursor:find("OtHidden") ~= nil, "no cursor in the list")
press("<CR>")
check(vim.o.guicursor:find("OtHidden") == nil, "your cursor back in the code")
vim.cmd.wincmd("h")
check(vim.o.guicursor:find("OtHidden") ~= nil, "and gone again back in the list")

-- Going somewhere in the code yourself leaves the list alone: line 12 is
-- in count, under report, and the list stays on report.
press("<CR>")
press("12G")
local on = api.nvim_buf_get_lines(api.nvim_win_get_buf(list_win), api.nvim_win_get_cursor(list_win)[1] - 1, api.nvim_win_get_cursor(list_win)[1], false)[1]
check(on:find("report") ~= nil, "the list stays on report, got: " .. on)
check(api.nvim_win_get_cursor(0)[1] == 12, "and the code stays on line 12")
-- Run headless, nvim fires CursorMoved only before the next key.
vim.cmd.doautocmd("CursorMoved")
vim.cmd.wincmd("h")
check(not faded(3), "off report in the code, nothing fades")
vim.cmd.wincmd("l")
press("8G")
vim.cmd.doautocmd("CursorMoved")
vim.cmd.wincmd("h")
check(faded(3), "and back on it, the rest fades again")

-- The list buffer never shows on the right, nor a file on the left.
local list_buf = api.nvim_win_get_buf(list_win)
check(not vim.bo[list_buf].buflisted, "the list is off the buffer list")
local code_win = code()
local file_buf = api.nvim_win_get_buf(code_win)
api.nvim_set_current_win(code_win)
vim.cmd("buffer " .. list_buf)
vim.wait(200, function()
  return api.nvim_win_get_buf(code_win) == file_buf
end, 10)
check(api.nvim_win_get_buf(code_win) == file_buf, "the list put in the code window goes back to the code, got " .. api.nvim_buf_get_name(api.nvim_win_get_buf(code_win)))
api.nvim_set_current_win(list_win)
vim.cmd("buffer " .. file_buf)
vim.wait(200, function()
  return api.nvim_win_get_buf(list_win) == list_buf and api.nvim_get_current_win() == code_win
end, 10)
check(api.nvim_win_get_buf(list_win) == list_buf, "a file put in the list window leaves the list there")
check(api.nvim_get_current_win() == code_win and api.nvim_win_get_buf(code_win) == file_buf, "and shows on the right instead, got win " .. api.nvim_get_current_win())
vim.cmd.wincmd("h")

press("t")
check(has(list(), "sum.test.ts"), "t shows the test file")
press("<Tab>")
l = list()
check(has(l, "^IN src/$"), "the files list names the directory they share")
check(has(l, "^  · sum.test.ts") and has(l, "^  · sum.ts"), "the files list holds both files")

-- Once the review checkout is there, the code is its real file, which
-- your LSP can work on, with the same marks drawn on it.
vim.wait(2000, function()
  return api.nvim_buf_get_name(api.nvim_win_get_buf(code())):find("/root/src/sum.ts$") ~= nil
end, 10)
check(api.nvim_buf_get_name(api.nvim_win_get_buf(code())):find("/root/src/sum.ts$") ~= nil, "the code becomes the checkout's real file")
check(vim.bo[api.nvim_win_get_buf(code())].buftype == "", "a real file is a normal buffer, which an LSP attaches to")
check(not vim.bo[api.nvim_win_get_buf(code())].modifiable, "someone else's MR can't be edited, so nothing of yours gets pushed to it")
check(not faded(3) and not faded(11), "a file's row shows all of it, nothing faded")

-- Removed lines and the thread hang off the lines they belong to.
local virt = {}
local buf = api.nvim_win_get_buf(code())
for _, m in ipairs(api.nvim_buf_get_extmarks(buf, -1, 0, -1, { details = true })) do
  for _, vl in ipairs(m[4].virt_lines or {}) do
    local text = ""
    for _, chunk in ipairs(vl) do
      text = text .. chunk[1]
    end
    virt[#virt + 1] = text
  end
end
check(has(virt, "function unused%(%) {"), "unused's removed lines are drawn")
check(has(virt, "┃ kim"), "kim's thread hangs under its line")
-- } goes to the change's comment from anywhere, and round again from the last.
api.nvim_win_set_cursor(code(), { 1, 0 })
press("}")
check(code_line() == 8, "} puts the code on kim's comment, line 8, not " .. code_line())
check(api.nvim_buf_get_name(api.nvim_win_get_buf(code())):find("sum.ts$") ~= nil, "} shows the comment's file")
check(has(list(), "comment 1 of 1"), "the list says which comment it is on")
press("}")
check(code_line() == 8, "} from the last comment goes round to the first")
-- A removed line wider than the code is cut into rows, as nvim wraps the
-- line under it; virtual lines never wrap on their own.
local function removed_rows()
  local rows = {}
  for _, m in ipairs(api.nvim_buf_get_extmarks(buf, -1, 0, -1, { details = true })) do
    for _, vl in ipairs(m[4].virt_lines or {}) do
      if vl[1][2] == "OtDel" or vl[1][2] == "OtDelWord" then
        local text = ""
        for _, chunk in ipairs(vl) do
          text = text .. chunk[1]
        end
        rows[#rows + 1] = (text:gsub("%s+$", ""))
      end
    end
  end
  return rows
end
api.nvim_win_set_width(list_win, 70)
vim.cmd.doautocmd("VimResized")
local info = vim.fn.getwininfo(code())[1]
local room, widest, cut = info.width - info.textoff, 0, false
for _, r in ipairs(removed_rows()) do
  widest = math.max(widest, vim.fn.strdisplaywidth(r))
  cut = cut or (r ~= "" and r ~= "function unused() {" and ("function unused() {"):sub(1, #r) == r)
end
check(room < 19 and widest <= room and cut, ("a narrow code cuts unused's removed line into rows of its width: room %d, widest %d"):format(room, widest))
api.nvim_win_set_width(list_win, 30)
vim.cmd.doautocmd("VimResized")
check(has(removed_rows(), "^function unused%(%) {$"), "and the line is whole again once the code is wide")

local function marks()
  local f = io.open(vim.fs.dirname(vim.env.OT_REVIEW) .. "/viewed", "r")
  local text = f and f:read("a") or ""
  if f then
    f:close()
  end
  return text, select(2, text:gsub("[^\n]+", ""))
end
local _, before = marks()
press("v")
local written, after = marks()
check(written:find("#file\t") ~= nil, "v writes the file's mark")
check(after - before > 1, ("and marks what is in the file with it: %d marks, then %d"):format(before, after))
press("v")
local unwritten, again = marks()
check(not unwritten:find("#file\t") and again == before, "v again unmarks the file and what is in it")
press("v")

-- Writing goes back to ot: a comment on line 8, a draft on someone else's
-- MR, then the review finished with an approval and a word.
local function list_lines()
  for _, b in ipairs(api.nvim_list_bufs()) do
    if api.nvim_buf_get_name(b):find("ot://!%d+$") then
      return api.nvim_buf_get_lines(b, 0, -1, false)
    end
  end
  return {}
end
vim.cmd.wincmd("l")
api.nvim_win_set_cursor(0, { 8, 0 })
-- The review's keys in the code are under <leader>m, which --clean leaves at \.
check(vim.fn.maparg("c", "n") == "" and vim.fn.maparg("A", "n") == "", "c and A are vim's in the code")
press("\\mc")
check(api.nvim_buf_get_name(0):find("^ot://box/") ~= nil, "<leader>mc opens a box")
api.nvim_buf_set_lines(0, 0, -1, false, { "why reduce?" })
vim.cmd.write()
vim.wait(2000, function()
  return has(list_lines(), "draft saved")
end, 10)
check(has(list_lines(), "draft saved"), "the box's :w saves the draft")
check(has(list_lines(), "1 draft not sent"), "and the list counts it")
local drawn = {}
for _, m in ipairs(api.nvim_buf_get_extmarks(0, -1, { 7, 0 }, { 7, -1 }, { details = true })) do
  for _, vl in ipairs(m[4].virt_lines or {}) do
    drawn[#drawn + 1] = vl[2] and vl[2][1] or ""
  end
end
check(has(drawn, "your draft, not sent yet") and has(drawn, "why reduce%?"), "the draft shows under line 8")
-- c on lines selected comments on all of them.
api.nvim_win_set_cursor(0, { 7, 0 })
press("Vj\\mc")
check(api.nvim_buf_get_name(0):find("^ot://box/") ~= nil, "<leader>mc on a selection opens a box")
api.nvim_buf_set_lines(0, 0, -1, false, { "these two" })
vim.cmd.write()
local function drafted(text)
  for _, m in ipairs(api.nvim_buf_get_extmarks(0, -1, { 7, 0 }, { 7, -1 }, { details = true })) do
    for _, vl in ipairs(m[4].virt_lines or {}) do
      if vl[2] and vl[2][1] == text then
        return true
      end
    end
  end
end
vim.wait(2000, function()
  return drafted("these two")
end, 10)
check(drafted("these two"), "the selection's draft shows under its last line")
-- With room under the list, the hills ot sends sit in a foot of their own.
local wins = #api.nvim_list_wins()
vim.o.lines = 40
vim.wait(500, function()
  return #api.nvim_list_wins() == wins + 1
end, 10)
local foot
for _, w in ipairs(api.nvim_list_wins()) do
  if api.nvim_win_get_height(w) == 9 then
    foot = w
  end
end
check(foot ~= nil, "a tall screen gives the paused music a foot of 9 rows under the list")
check(foot and api.nvim_buf_get_lines(api.nvim_win_get_buf(foot), 0, 1, false)[1] == " ▂ ▇ ", "the foot shows the hills ot drew")
check(foot and api.nvim_win_get_position(foot)[2] == 0 and api.nvim_win_get_position(foot)[1] > 1, "the foot is under the list, at the left")
-- Approving is the list's: in the code, A is vim's.
vim.cmd.wincmd("h")
press("A")
check(api.nvim_get_current_line():find("approve !301 with your 1 draft%?  y yes  m with a note") ~= nil, "A asks how to approve first, got " .. api.nvim_get_current_line())
press("m")
api.nvim_buf_set_lines(0, 0, -1, false, { "lgtm" })
vim.cmd.write()
vim.wait(2000, function()
  return has(list_lines(), "approved")
end, 10)
check(has(list_lines(), "approved"), "A finishes the review with an approval")

-- Claude's tour is a page of its own: a row in the list, read from top to
-- bottom, n and p going step to step and enter into a step's file.
for _, w in ipairs(api.nvim_list_wins()) do
  if api.nvim_buf_get_name(api.nvim_win_get_buf(w)):find("ot://!%d+$") then
    api.nvim_set_current_win(w)
  end
end
local function showing(name)
  for _, w in ipairs(api.nvim_list_wins()) do
    if api.nvim_buf_get_name(api.nvim_win_get_buf(w)):find(name) then
      return w
    end
  end
end
local function page()
  local w = showing("the tour$")
  return w and api.nvim_buf_get_lines(api.nvim_win_get_buf(w), 0, -1, false) or {}
end
local function land(tour)
  vim.fn.writefile({ vim.json.encode(tour) }, vim.env.OT_TOUR)
end
local code_of = { { mark = " ", n = 7, text = "export function report(items: number[]) {" }, { mark = "-", text = "  return 1;" }, { mark = "+", n = 8, text = "  return total(items);" } }
-- It lands while you read a file, so only its row appears.
land({ reading = true, summary = "", cases = {}, steps = {} })
vim.wait(2000, function()
  return has(list_lines(), "The tour +Claude is writing…")
end, 10)
check(has(list_lines(), "The tour +Claude is writing…"), "the list has the tour's row while Claude writes")
check(showing("the tour$") == nil, "a tour landing while you read a file moves nothing")
press("n")
check(has(page(), "Claude is reading the change"), "n shows the tour's page, which says Claude is reading")
-- Something runs over that line meanwhile, drawn beside the text.
local function machine()
  local w = showing("the tour$")
  for _, m in ipairs(w and api.nvim_buf_get_extmarks(api.nvim_win_get_buf(w), api.nvim_create_namespace("ot.wait"), 0, -1, { details = true }) or {}) do
    return #m[4].virt_lines
  end
end
vim.wait(2000, machine, 10)
check((machine() or 0) > 6, "a machine runs on the page while Claude reads, got " .. tostring(machine()))
land({
  summary = "The report says how many items there were.",
  cases = { { when = "A report of no items", before = "It said total 0.", now = "It says total 0 of 0." } },
  steps = {
    { file = 2, path = "src/sum.ts", line = 8, title = "The report counts", says = "The report counts too.", ask = "Is one call enough?", beyond = 3, code = code_of },
    { file = 2, path = "src/sum.ts", line = 4, says = "Summing is one call now.", code = {} },
  },
  flags = { { file = 2, path = "src/sum.ts", line = 8, title = "One thing, two names", says = "It is total here and count there.", instead = "Call both total.", code = code_of } },
})
vim.wait(2000, function()
  return has(list_lines(), "The tour +2 steps, 1 ⚑")
end, 10)
check(has(list_lines(), "The tour +2 steps, 1 ⚑"), "the tour's row counts its steps and flags")
check(has(page(), "^⚑ One thing, two names$") and has(page(), "^## ⚑  One thing, two names$") and #vim.tbl_filter(function(l) return l == "It is total here and count there." or l == "Call both total." end, page()) == 4, "a design flag says what is wrong and what to do instead before the walk, and again with its code after it")
check(machine() == nil, "the machine stops when the tour lands")
check(has(page(), "A report of no items") and has(page(), "^  It says total 0 of 0%."), "the page tells the cases that end differently")
check(has(page(), "^```typescript$") and has(page(), "^  return total%(items%);$") and has(page(), "^  return 1;$"), "a step is over its own code, in its language, got " .. table.concat(page(), " / "))
check(has(page(), "^Is one call enough%?$") and has(page(), "^3 more changed lines here"), "a step's question and what its code leaves out are said")
local function page_line()
  return api.nvim_buf_get_lines(api.nvim_win_get_buf(showing("the tour$")), api.nvim_win_get_cursor(showing("the tour$"))[1] - 1, api.nvim_win_get_cursor(showing("the tour$"))[1], false)[1]
end
press("n")
check(page_line() == "## 1  The report counts", "n goes to the first step, got " .. page_line())
press("n")
check(page_line() == "## 2  sum.ts", "n again goes to the second, got " .. page_line())
press("n")
check(page_line() == "## ⚑  One thing, two names", "n after the last step goes to the first flag, got " .. page_line())
press("n")
check(page_line() == "## ⚑  One thing, two names", "n on the last flag stays")
press("p")
press("p")
check(page_line() == "## 1  The report counts", "p goes back a step, got " .. page_line())
-- enter on the page goes into the step's file, the step said over its line.
api.nvim_set_current_win(showing("the tour$"))
press("<CR>")
local file = showing("sum%.ts$")
local over
for _, m in ipairs(file and api.nvim_buf_get_extmarks(api.nvim_win_get_buf(file), -1, { 7, 0 }, { 7, -1 }, { details = true }) or {}) do
  for _, row in ipairs(m[4].virt_lines or {}) do
    over = row[1][1] == "┆ " and row[2][1] or over
  end
end
check(file and api.nvim_win_get_cursor(file)[1] == 8 and over == "The report counts too.", "enter goes into the step's file at its line, under " .. tostring(over))
press("\\mn")
check(showing("the tour$") ~= nil and page_line() == "## 2  sum.ts", "<leader>mn in the code goes back to the tour, at the next step")

check(in_float == 0, "no file loaded in a float, got " .. in_float)

if #failed > 0 then
  io.stderr:write(table.concat(failed, "\n") .. "\n")
  vim.cmd("cquit 1")
end
vim.cmd("qa!")
