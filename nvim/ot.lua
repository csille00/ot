-- ot's review in nvim. ot works out everything shown here (src/nvim.rs) and
-- writes it as JSON; this file only draws it and answers keys, so the logic
-- stays in Rust where it is tested. Indexes and line numbers count from 1.
local M = {}
local api, uv = vim.api, vim.uv
local marks_ns = api.nvim_create_namespace("ot")
local here_ns = api.nvim_create_namespace("ot.here")
local list_ns = api.nvim_create_namespace("ot.list")
local band_ns = api.nvim_create_namespace("ot.band")
local W = vim.fn.strdisplaywidth

-- Defined at the end, used before: the keys mapped in the list and in a
-- file's buffer, the line that shows them, the code's winbar, and the
-- overview's buffer.
local list_acts, code_acts, player_acts, tour_acts, keys_line, key_words, winbar, overview, tour_page, prose, open_thread_pane

-- The one review this nvim shows. Only what the view itself decides lives
-- here: what is open, which list, where the cursor is.
local S = {
  d = nil, -- what ot sent, once it lands
  note = nil, -- what is happening while it hasn't, or what went wrong
  threads = {},
  drafts = {}, -- yours, not sent yet
  foot = nil, -- the window of hills under the list, while there is one
  hill_hl = {}, -- the hills' colors, as highlight groups made for them
  description = "",
  open_threads = {}, -- symbol index -> its open threads, as ot counts them
  say = nil, -- how the last thing you did went, while it matters: { text, hl }
  overview = nil, -- the buffer "The change" shows, and what each line is
  overview_at = {},
  uploads = {}, -- a body's "/uploads/…" -> what ot fetched of it, false while asked
  open = {}, -- symbol rows opened, by their path ("3/7")
  tab = "symbols",
  aside = false,
  viewed = {},
  at = nil, -- the id of the row the cursor rests on
  rows = {}, -- list buffer line -> row
  line = {}, -- row id -> its line in the list
  shown = nil, -- the row the code shows
  faded = false, -- whether draw() faded the code round it; bar() redraws when that flips
  bufs = {}, -- file index -> its buffer
  tabs = {}, -- list name -> { at, topline } as you left it
  tour = nil, -- Claude's tour (src/nvim.rs tour_json) when the review was opened for one, as far as it is written while `reading`, or its error
  tourbuf = nil, -- the page it is read on
  wait = nil, -- what runs on it while Claude reads: { timer, kind, machine, cols, rows, line }
  tour_stops = {}, -- its steps, then its design flags, as the page has them
  tour_first = {}, -- each one's first line on that page
  step = nil, -- the step you went into the code from
  root = nil, -- ot's review checkout, once it is at the MR's head
  tree_note = nil, -- what the checkout is doing, or why it isn't there
  music = nil, -- what cliamp plays, as ot writes it (src/nvim.rs music)
  player = nil, -- the window M opens on it, while open
}

local function player_open()
  return S.player ~= nil and api.nvim_win_is_valid(S.player)
end

local function colors()
  local set = function(name, v) api.nvim_set_hl(0, name, v) end
  set("OtText", { fg = "#ebdbb2" })
  set("OtTitle", { fg = "#ebdbb2", bold = true })
  set("OtDim", { fg = "#a89984" })
  set("OtFaint", { fg = "#665c54" })
  set("OtOrange", { fg = "#fe8019" })
  set("OtRed", { fg = "#fb4934" })
  set("OtGreen", { fg = "#b8bb26" })
  set("OtTeal", { fg = "#83a598" })
  set("OtBand", { bg = "#32302f" })
  set("OtAddHere", { bg = "#3f5a45" })
  set("OtUses", { underline = true, sp = "#83a598" })
  set("OtHidden", { blend = 100, nocombine = true })
  set("OtPink", { fg = "#d3869b" })
  set("OtAmber", { fg = "#fabd2f" })
  set("OtAdd", { bg = "#26332a" })
  -- The words a changed line changed: the band's color, brighter.
  set("OtAddWord", { bg = "#4a7a38" })
  set("OtDel", { fg = "#ebdbb2", bg = "#3c2022" })
  -- A removed line's band where the code keeps its own colors.
  set("OtDelBand", { bg = "#3c2022" })
  set("OtDelWordBand", { bg = "#8a3a40" })
  set("OtDelFaint", { fg = "#665c54", bg = "#3c2022" })
  set("OtDelWord", { fg = "#ebdbb2", bg = "#8a3a40" })
end

local function read(path)
  local f = io.open(path, "r")
  if not f then
    return nil
  end
  local s = f:read("a")
  f:close()
  local ok, v = pcall(vim.json.decode, s, { luanil = { object = true, array = true } })
  return ok and v or nil
end

-- Hands got() the file's JSON each time the file changes, until got() says
-- it has what it waited for. ot writes the files while nvim starts.
local function watch(path, got)
  local timer, last = uv.new_timer(), nil
  timer:start(
    0,
    15,
    vim.schedule_wrap(function()
      local st = path and uv.fs_stat(path)
      if not st or timer:is_closing() then
        return
      end
      local key = ("%d.%d:%d"):format(st.mtime.sec, st.mtime.nsec, st.size)
      if key == last then
        return
      end
      last = key
      local v = read(path)
      if v and got(v) and not timer:is_closing() then
        timer:stop()
        timer:close()
      end
    end)
  )
end

-- Text in lines no wider than w, never cut: at a space where the line has
-- one, else after - / _ . or between the words of a camelCase name, and
-- inside a word only when it must.
local function wrap(text, w)
  w = math.max(w, 4)
  local out, rest = {}, vim.trim(text)
  while W(rest) > w do
    local fit, space, name, last, at = 0, nil, nil, " ", 1
    for ch in rest:gmatch("[%z\1-\127\194-\244][\128-\191]*") do
      local stop = at + #ch - 1
      if W(rest:sub(1, stop)) > w then
        break
      end
      if ch:match("^%u$") and last:match("^[%l%d]$") and at > 1 then
        name = at - 1
      end
      fit = stop
      if ch == " " then
        space = stop
      elseif ch:match("^[-/_.]$") then
        name = stop
      end
      last, at = ch, stop + 1
    end
    local cut = space or name or math.max(fit, 1)
    out[#out + 1] = (rest:sub(1, cut):gsub("%s+$", ""))
    rest = (rest:sub(cut + 1):gsub("^%s+", ""))
  end
  out[#out + 1] = rest
  return out
end

-- How wide a list of { text, highlight } draws.
local function width(segs)
  local n = 0
  for _, g in ipairs(segs) do
    n = n + W(g[1])
  end
  return n
end

-- Chunks, a list of { text, highlight }, in rows no wider than w, cut
-- where nvim cuts a buffer line of that width, each highlight carried
-- over the cut. For a removed line, which nvim would otherwise draw past
-- the window's edge: virtual lines never wrap.
local function cut(chunks, w)
  local rows, row, used = {}, {}, 0
  for _, c in ipairs(chunks) do
    local piece = ""
    for ch in c[1]:gmatch("[%z\1-\127\194-\244][\128-\191]*") do
      local cw = W(ch)
      if used + cw > w then
        if piece ~= "" then
          row[#row + 1] = { piece, c[2] }
        end
        rows[#rows + 1] = row
        row, used, piece = {}, 0, ""
      end
      piece, used = piece .. ch, used + cw
    end
    if piece ~= "" then
      row[#row + 1] = { piece, c[2] }
    end
  end
  rows[#rows + 1] = row
  return rows
end

-- Pieces, each a list of { text, highlight }, in lines no wider than w with
-- sep between two on a line; a piece breaks only when it is wider than a
-- line by itself.
local function pack(pieces, w, sep)
  local out, used = {}, 0
  for _, piece in ipairs(pieces) do
    local pw = width(piece)
    if #out > 0 and used + W(sep[1]) + pw <= w then
      table.insert(out[#out], sep)
      vim.list_extend(out[#out], piece)
      used = used + W(sep[1]) + pw
    elseif pw <= w then
      out[#out + 1] = vim.list_extend({}, piece)
      used = pw
    else
      local text = ""
      for _, g in ipairs(piece) do
        text = text .. g[1]
      end
      for _, l in ipairs(wrap(text, w)) do
        out[#out + 1] = { { l, piece[1][2] } }
      end
      used = W(out[#out][1][1])
    end
  end
  return out
end

local function thing(r)
  if r.kind == "overview" or r.kind == "tour" then
    return { hunks = {}, counts = "" }
  elseif r.kind == "symbol" then
    return S.d.symbols[r.i]
  elseif r.kind == "loose" then
    return S.d.loose[r.i]
  elseif r.kind == "file" then
    return S.d.files[r.i]
  end
end

local function resting(r)
  return r and (r.kind == "overview" or r.kind == "tour" or r.kind == "symbol" or r.kind == "loose" or r.kind == "file")
end

local function shown(f)
  return S.aside or not S.d.files[f].aside
end

-- The directory every path is under, with its slash; "" for none.
local function shared_dir()
  local first = S.d.files[1] and S.d.files[1].path or ""
  local dir = first:match("^(.*/)") or ""
  for _, f in ipairs(S.d.files) do
    while dir ~= "" and f.path:sub(1, #dir) ~= dir do
      dir = dir:sub(1, -2):match("^(.*/)") or ""
    end
  end
  return dir
end

-- What the list shows, top to bottom; everything starts closed. Symbols
-- hang off "The change" with the board's rails.
local function rows()
  local out = {}
  local function add(r)
    out[#out + 1] = r
  end
  local function visible(list)
    return vim.tbl_filter(function(i)
      return shown(S.d.symbols[i].file)
    end, list)
  end
  if S.tour then
    add({ kind = "tour", id = "tour" })
  end
  if S.tab == "symbols" then
    add({ kind = "overview", id = "overview" })
    local function walk(path, parent, prefix, last)
      local i = path[#path]
      local key = table.concat(path, "/")
      local all = visible(S.d.symbols[i].kids)
      local kids = S.open[key] and all or {}
      -- Roots stand on their own; rails run only from an open row down to
      -- what it opens to.
      local root = #path == 1
      local under = root and "" or prefix .. (last and "   " or "│  ")
      add({
        kind = "symbol", id = "s:" .. key, key = key, parent = parent, i = i,
        rails = root and "" or prefix .. (last and "└─ " or "├─ "),
        -- What it opens to, as many as the list would show.
        under = #all,
      })
      for j, k in ipairs(kids) do
        local p = vim.list_extend({}, path)
        p[#p + 1] = k
        walk(p, key, under, j == #kids)
      end
    end
    local roots = visible(S.d.roots)
    for j, r in ipairs(roots) do
      walk({ r }, nil, "", j == #roots)
    end
    local loose = {}
    for i, l in ipairs(S.d.loose) do
      if shown(l.file) then
        loose[#loose + 1] = { kind = "loose", id = "l:" .. i, i = i, rails = "" }
      end
    end
    if #loose > 0 then
      add({ kind = "heading", text = "" })
      add({ kind = "heading", text = "FILES WITH CHANGES OUTSIDE ONE" })
      vim.list_extend(out, loose)
    end
  else
    local dir = shared_dir()
    if dir ~= "" then
      add({ kind = "heading", text = "IN " .. dir })
    end
    for f in ipairs(S.d.files) do
      if shown(f) then
        add({ kind = "file", id = "f:" .. f, i = f, dir = dir, rails = "" })
      end
    end
  end
  local aside, tests = 0, true
  for _, f in ipairs(S.d.files) do
    if f.aside then
      aside = aside + 1
      tests = tests and f.kind == "tests"
    end
  end
  if aside > 0 then
    add({ kind = "heading", text = "" })
    add({ kind = "aside", n = aside, what = (tests and "test " or "test and generated ") .. (aside == 1 and "file" or "files") })
  end
  return out
end


local function tally()
  local seen, all = 0, 0
  local function count(t)
    if shown(t.file) then
      all = all + 1
      seen = seen + (S.viewed[t.mark] and 1 or 0)
    end
  end
  for _, t in ipairs(S.tab == "symbols" and S.d.symbols or S.d.files) do
    count(t)
  end
  if S.tab == "symbols" then
    for _, l in ipairs(S.d.loose) do
      count(l)
    end
  end
  return seen, all
end

local function set_lines(buf, lines, hls)
  vim.bo[buf].modifiable = true
  api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modifiable = false
  api.nvim_buf_clear_namespace(buf, list_ns, 0, -1)
  for l, hs in pairs(hls) do
    for _, h in ipairs(hs) do
      api.nvim_buf_set_extmark(buf, list_ns, l - 1, h[1], { end_col = h[2], hl_group = h[3] })
    end
  end
end

-- A name's colour by what it is: functions green, classes orange, types
-- amber, the rest (consts, fields) teal.
local KIND_HL = {
  fn = "OtGreen", constfn = "OtGreen", method = "OtGreen", macro = "OtGreen",
  class = "OtOrange", trait = "OtOrange", impl = "OtOrange",
  interface = "OtAmber", type = "OtAmber", enum = "OtAmber", struct = "OtAmber",
}

-- A path in n columns if it can be: its leading folders give way to "…/"
-- one at a time, never its file's name.
local function shorten(path, n)
  local short, rest = path, path:match("^[^/]*/(.*)$")
  while rest and W(short) > n do
    short, rest = "…/" .. rest, rest:match("^[^/]*/(.*)$")
  end
  return short
end

-- A symbol's or a file's row as drawn in room columns: its rails, whether
-- it opens, what the change did to it (+ added, − removed, · changed), its
-- name coloured by what it is, and how much is under it when closed. The
-- columns at its right (open threads, lines added, lines removed, viewed)
-- come back as plain strings, since render lines them up across the rows.
local function cells(r, room)
  local t = thing(r)
  local viewed = S.viewed[t.mark]
  local fold, did, name, threads = { "  " }, { "· ", "OtFaint" }, {}, 0
  if r.kind == "symbol" then
    if r.under > 0 then
      fold = { S.open[r.key] and "▾ " or "▸ ", "OtAmber" }
    end
    if t.moved and t.lines then
      did = { "→ ", "OtTeal" }
    elseif not t.old then
      did = { "+ ", "OtGreen" }
    elseif not t.lines then
      did = { "− ", "OtDim" }
    end
    local hl = (viewed or not t.lines) and "OtDim" or KIND_HL[t.kind] or "OtTeal"
    name = { { t.name, hl } }
    -- Where it moved from, or went: a path gives way, never the name.
    if t.moved then
      local path = shorten(t.moved, room - width({ { r.rails }, fold, did, name[1] }) - 6)
      name[#name + 1] = { (t.lines and " from " or " to ") .. path, "OtFaint" }
    end
    if r.under > 0 and not S.open[r.key] then
      name[#name + 1] = { " " .. r.under, "OtFaint" }
    end
    threads = S.open_threads[r.i] or 0
  else
    -- A path's folder dim, its name lit. Its folders give way to the room
    -- the symbols leave, so a deep path can't push the code off the screen;
    -- the code's bar shows it whole.
    local path = r.kind == "loose" and S.d.files[t.file].path or t.path:sub(#r.dir + 1)
    local dir, base = shorten(path, room - width({ { r.rails }, fold, did })):match("^(.-)([^/]*)$")
    name = { { dir, "OtDim" }, { base, viewed and "OtDim" or "OtText" } }
  end
  local left = { { r.rails, "OtFaint" }, fold, did }
  vim.list_extend(left, name)
  return left, { threads = threads > 0 and tostring(threads) or "", adds = t.adds, dels = t.dels, viewed = viewed }
end

-- The right-hand columns of a row, each as wide as the list's widest
-- (wide: the widest of each, keyed as the cells are).
local function right_cells(c, wide)
  local function pad(s, n)
    return (" "):rep(n - W(s)) .. s
  end
  local out = { { pad(c.threads, wide.threads), "OtOrange" }, { "  " } }
  out[#out + 1] = { pad(c.adds, wide.adds), "OtGreen" }
  if wide.dels > 0 then
    out[#out + 1] = { "  " }
    out[#out + 1] = { pad(c.dels, wide.dels), "OtRed" }
  end
  -- The viewed column is there before anything is viewed, so the first ✓
  -- shifts nothing.
  out[#out + 1] = { c.viewed and " ✓" or "  ", "OtGreen" }
  return out
end

-- The list's lines, laid out here rather than wrapped by nvim, so each row
-- is one line with its counts at the right edge.
local function render()
  if not S.d then
    set_lines(S.listbuf, { S.note or "" }, { { { 0, #(S.note or ""), "OtDim" } } })
    return
  end
  -- Every name whole on one line: the list widens to its widest row, each
  -- row at its narrowest (a file down to its name), and never narrows
  -- while the review is open.
  local list, right, widest = rows(), {}, 0
  -- The threads land after the list: their column is kept open for them
  -- from the start, so the divider stays put when they come.
  local said, wide = {}, { threads = 1, adds = 0, dels = 0 }
  for i, r in ipairs(list) do
    if resting(r) and r.kind ~= "overview" and r.kind ~= "tour" then
      said[i] = { cells(r, 0) }
      local c = said[i][2]
      wide.threads, wide.adds, wide.dels = math.max(wide.threads, W(c.threads)), math.max(wide.adds, W(c.adds)), math.max(wide.dels, W(c.dels))
    end
  end
  for i, s in pairs(said) do
    right[i] = right_cells(s[2], wide)
    widest = math.max(widest, width(s[1]) + 1 + width(right[i]))
  end
  -- The text area, whatever gutter your config gives the window, and a
  -- column spare at its right.
  local info = vim.fn.getwininfo(S.list)[1]
  -- Grown to fit, but never past 60% of the screen: the code needs the rest.
  local most = math.floor(vim.o.columns * 0.6)
  if info.width - info.textoff - 1 < widest and info.width < most then
    api.nvim_win_set_width(S.list, math.min(widest + info.textoff + 1, most))
    info = vim.fn.getwininfo(S.list)[1]
  end
  local w = info.width - info.textoff - 1
  local lines, hls, map, line = {}, {}, {}, {}
  local function put(segs, r)
    local s, h = "", {}
    for _, g in ipairs(segs) do
      if g[2] then
        h[#h + 1] = { #s, #s + #g[1], g[2] }
      end
      s = s .. g[1]
    end
    lines[#lines + 1] = s
    hls[#lines] = h
    map[#lines] = r
    if r and r.id then
      line[r.id] = #lines
    end
  end
  local function text(t, hl)
    for _, l in ipairs(wrap(t, w)) do
      put({ { l, hl } })
    end
  end
  local d = S.d
  text(d.name .. "  " .. d.title, "OtTitle")
  local seen, all = tally()
  local facts = { { { d.author, "OtDim" } }, { { d.size, "OtDim" } }, { { ("%d of %d viewed"):format(seen, all), seen == all and "OtGreen" or "OtDim" } } }
  if d.merge then
    table.insert(facts, 1, { { d.merge.word, d.merge.hl } })
  end
  if #S.drafts > 0 then
    facts[#facts + 1] = { { ("%d %s not sent"):format(#S.drafts, #S.drafts == 1 and "draft" or "drafts"), "OtPink" } }
  end
  for _, l in ipairs(pack(facts, w, { "  ·  ", "OtFaint" })) do
    put(l)
  end
  if S.threads_error then
    text(S.threads_error, "OtDim")
  end
  if S.tree_note then
    text(S.tree_note, "OtDim")
  end
  if S.say then
    text(S.say[1], S.say[2])
  end
  for _, l in ipairs(pack(keys_line("List", list_acts), w, { "  " })) do
    put(l)
  end
  -- What plays, in a line, unless the player shows it whole.
  local m = S.music
  if m and m.title ~= "" and not player_open() then
    local pieces = { { { "♪ " .. m.title, "OtTeal" } } }
    if m.artist ~= "" then
      pieces[#pieces + 1] = { { m.artist, "OtDim" } }
    end
    local state = m.buffering and "buffering" or m.state ~= "playing" and m.state or nil
    if state then
      pieces[#pieces + 1] = { { state, "OtAmber" } }
    end
    for _, l in ipairs(pack(pieces, w, { "  ·  ", "OtFaint" })) do
      put(l)
    end
  end
  put({ { "" } })
  for i, r in ipairs(list) do
    if r.kind == "tour" then
      local t = S.tour
      local flags = #(t.flags or {})
      local tail = t.error and "none" or t.reading and "Claude is writing…" or ("%d steps"):format(#t.steps) .. (flags > 0 and (", %d ⚑"):format(flags) or "")
      put({ { "The tour", "OtTeal" }, { (" "):rep(math.max(1, w - 8 - W(tail))) }, { tail, (t.error and "OtAmber") or (flags > 0 and not t.reading and "OtRed") or "OtDim" } }, r)
    elseif r.kind == "overview" then
      local n = 0
      for _, t in ipairs(S.threads) do
        n = n + ((not t.file and t.resolvable and not t.resolved) and 1 or 0)
      end
      local tail = n > 0 and tostring(n) or ""
      put({ { "The change", "OtText" }, { (" "):rep(math.max(1, w - 10 - #tail)) }, { tail, "OtOrange" } }, r)
    elseif r.kind == "heading" then
      for _, l in ipairs(wrap(r.text, w)) do
        put({ { l, "OtDim" } }, r)
      end
    elseif r.kind == "aside" then
      local said = ("%s %d %s"):format(S.aside and "▾" or "▸", r.n, r.what:upper())
      put({ { said, "OtDim" }, { ("   t %s them"):format(S.aside and "hides" or "shows"), "OtFaint" } }, r)
    else
      local left = cells(r, w - 1 - width(right[i]))
      local gap = { (" "):rep(math.max(1, w - width(left) - width(right[i]))) }
      put(vim.list_extend(vim.list_extend(left, { gap }), right[i]), r)
    end
  end
  -- The list keeps its scroll through a redraw; the cursor's row is kept
  -- in view below.
  local top = api.nvim_win_call(S.list, vim.fn.winsaveview).topline
  set_lines(S.listbuf, lines, hls)
  S.rows, S.line = map, line
  -- The cursor back on its row, else on the first it can rest on.
  local target = S.at and line[S.at]
  if not target then
    for l = 1, #lines do
      if resting(map[l]) then
        target = l
        break
      end
    end
  end
  if target then
    api.nvim_win_call(S.list, function()
      vim.fn.winrestview({ topline = math.min(top, #lines), lnum = target, col = 0 })
    end)
  end
  M.follow()
end

-- The symbol the list shows, while the code's cursor is on it: on its
-- lines, or on a hunk of its that hangs off another's.
local function on_shown()
  local sym = S.shown and S.shown.kind == "symbol" and thing(S.shown)
  if not sym or S.bufs[sym.file] ~= api.nvim_win_get_buf(S.code) then
    return nil
  end
  local at = api.nvim_win_get_cursor(S.code)[1]
  if sym.lines and sym.lines[1] <= at and at <= sym.lines[2] then
    return sym
  end
  for _, h in ipairs(sym.hunks) do
    if h.at <= at and at <= h.last then
      return sym
    end
  end
end

-- What a hunk or a thread hangs on, drawn into the file the code window
-- shows: when it comes into the window, when threads land, and when the
-- window is resized. Only the shown file, since its removed lines are cut
-- to the width the window gives it, which only the window knows. The
-- window set for code first: numbered, and wrapped plainly at its edge,
-- where cut wraps the removed lines (your linebreak, breakindent and
-- showbreak would wrap the added lines elsewhere), with a gutter of one
-- fixed width so the wrap's column holds as signs come and go. The
-- overview sets the window for prose instead.
-- The buffer holds the text the review read, which its line numbers are
-- for. Your edits on your own MR move lines from under them: the marks
-- already drawn move with the text, and none are drawn on it again.
-- Step i of the tour as the rows drawn over its line in the file, no
-- wider than room.
local function step_rows(i, room)
  local out = {}
  for _, l in ipairs(wrap(S.tour_stops[i].says, room - 2)) do
    out[#out + 1] = { { "┆ ", "OtTeal" }, { l, "OtText" } }
  end
  return out
end

local function as_read(buf, file)
  local lines = api.nvim_buf_get_lines(buf, 0, -1, false)
  if #lines ~= #file.text then
    return false
  end
  for i, l in ipairs(lines) do
    if l ~= file.text[i] then
      return false
    end
  end
  return true
end

local function draw()
  local sym = on_shown()
  S.faded = sym ~= nil
  local buf, f = api.nvim_win_get_buf(S.code), nil
  for k, b in pairs(S.bufs) do
    if b == buf then
      f = k
    end
  end
  if not f then
    return
  end
  local wo = vim.wo[S.code]
  wo.number, wo.relativenumber, wo.signcolumn = true, false, "yes"
  wo.wrap, wo.linebreak, wo.breakindent, wo.showbreak = true, false, false, ""
  local file = S.d.files[f]
  if not as_read(buf, file) then
    return
  end
  api.nvim_buf_clear_namespace(buf, marks_ns, 0, -1)
  local info = vim.fn.getwininfo(S.code)[1]
  local room = info.width - info.textoff
  local last = api.nvim_buf_line_count(buf)
  local function mark(line, col, opts)
    api.nvim_buf_set_extmark(buf, marks_ns, math.min(line, last) - 1, col, opts)
  end
  -- On the symbol shown, the rest of its file fades: its new lines by
  -- number, its removed ones by their old number, since they can hang off
  -- another symbol's line. A gone file shows old lines.
  local new, old = { 1, math.huge }, { 1, math.huge }
  if sym then
    old = sym.old or { 1, 0 }
    new = file.gone and old or sym.lines or { 1, 0 }
  end
  local function faint(range, l)
    return l < range[1] or l > range[2]
  end
  local function fade(rows)
    for _, row in ipairs(rows) do
      for _, c in ipairs(row) do
        c[2] = { c[2], "OtFaint" }
      end
    end
    return rows
  end
  local tab = (" "):rep(vim.bo[buf].tabstop)
  local function untab(s)
    return (s:gsub("\t", tab))
  end
  local replies = {}
  for _, d in ipairs(S.drafts) do
    if d.reply_to then
      replies[d.reply_to] = replies[d.reply_to] or {}
      table.insert(replies[d.reply_to], d)
    end
  end
  local function talk(t, lead)
    local out = {}
    if t.body then
      out[#out + 1] = { { lead .. "┃ ", "OtPink" }, { "your draft, not sent yet", "OtPink" } }
      for _, para in ipairs(vim.split(t.body, "\n", { plain = true })) do
        for _, l in ipairs(wrap(para, room - 2)) do
          out[#out + 1] = { { lead .. "┃ ", "OtPink" }, { l, "OtText" } }
        end
      end
      return out
    end
    for _, d in ipairs(replies[t.id] or {}) do
      vim.list_extend(out, talk(d, lead))
    end
    local mine = out
    out = {}
    if t.resolved then
      local n = #t.notes
      local who = t.notes[1] and t.notes[1].author or ""
      return vim.list_extend({ { { lead .. "┃ ", "OtFaint" }, { ("resolved: %s, %d %s"):format(who, n, n == 1 and "note" or "notes"), "OtFaint" } } }, mine)
    end
    for _, note in ipairs(t.notes) do
      out[#out + 1] = { { lead .. "┃ ", "OtOrange" }, { note.author, "OtOrange" } }
      for _, para in ipairs(vim.split(note.body, "\n", { plain = true })) do
        for _, l in ipairs(wrap(para, room - 2)) do
          out[#out + 1] = { { lead .. "┃ ", "OtOrange" }, { l, "OtText" } }
        end
      end
    end
    return vim.list_extend(out, mine)
  end
  local under, after_old = {}, {}
  for _, d in ipairs(S.drafts) do
    local where = d.line and under or after_old
    local at = d.line or d.old
    if not d.reply_to and d.file == f and at then
      where[at] = where[at] or {}
      table.insert(where[at], d)
    end
  end
  for _, t in ipairs(S.threads) do
    if t.file == f and t.line then
      under[t.line] = under[t.line] or {}
      table.insert(under[t.line], t)
    elseif t.file == f and t.old then
      after_old[t.old] = after_old[t.old] or {}
      table.insert(after_old[t.old], t)
    end
  end
  if file.gone then
    local moved = {}
    for _, l in ipairs(file.moved) do
      moved[l] = true
    end
    for l = 1, #file.text do
      if not moved[l] then
        mark(l, 0, { line_hl_group = faint(new, l) and "OtDelFaint" or "OtDel" })
      end
    end
  end
  -- A band to the line's end, not a line highlight: nvim paints a line
  -- highlight's color over any word's on it, whatever their priorities.
  for _, a in ipairs(file.added) do
    mark(a.line, 0, { end_row = a.line, hl_group = "OtAdd", hl_eol = true, strict = false, priority = 10 })
    if a.words and a.words[2] > a.words[1] then
      mark(a.line, a.words[1], { end_col = a.words[2], hl_group = "OtAddWord", priority = 200 })
    end
  end
  -- Over the code's own colors, leaving its bands.
  if new[1] > 1 then
    mark(1, 0, { end_row = new[1] - 1, hl_group = "OtFaint", priority = 300 })
  end
  if new[2] < last then
    mark(new[2] + 1, 0, { end_row = last, hl_group = "OtFaint", priority = 300, strict = false })
  end
  -- A removed line's band runs past any window's edge, where nvim clips it,
  -- so it never has to be measured.
  local band = { (" "):rep(400), "OtDel" }
  for _, b in ipairs(file.removed) do
    local virt = {}
    for _, l in ipairs(b.lines) do
      local chunks, t = {}, l.text
      if l.words then
        chunks[#chunks + 1] = { untab(t:sub(1, l.words[1])), "OtDel" }
        chunks[#chunks + 1] = { untab(t:sub(l.words[1] + 1, l.words[2])), "OtDelWord" }
        chunks[#chunks + 1] = { untab(t:sub(l.words[2] + 1)), "OtDel" }
      else
        chunks[#chunks + 1] = { untab(t), "OtDel" }
      end
      local rows = cut(chunks, room)
      if faint(old, l.old) then
        fade(rows)
      end
      for _, row in ipairs(rows) do
        row[#row + 1] = band
        virt[#virt + 1] = row
      end
      for _, th in ipairs(after_old[l.old] or {}) do
        local said = talk(th, "")
        vim.list_extend(virt, faint(old, l.old) and fade(said) or said)
      end
    end
    mark(b.at, 0, { virt_lines = virt, virt_lines_above = not b.below })
  end
  for line, ts in pairs(under) do
    local virt = {}
    for _, t in ipairs(ts) do
      vim.list_extend(virt, talk(t, ""))
    end
    mark(line, 0, { virt_lines = faint(new, line) and fade(virt) or virt })
  end
  -- The step you came into the code from, said over its line.
  local s = S.step and S.tour_stops[S.step]
  if s and s.file == f then
    mark(s.line, 0, { virt_lines = step_rows(S.step, room), virt_lines_above = true })
  end
end

-- 'cursorline' draws under every mark, so on an added line the cursor's
-- bar is that line's green, a shade up: a mark that moves with the cursor,
-- a band over the line's band and under its words.
local cursor_ns = api.nvim_create_namespace("ot.cursor")
local function bar()
  if (on_shown() ~= nil) ~= S.faded then
    draw()
  end
  local buf = api.nvim_win_get_buf(S.code)
  api.nvim_buf_clear_namespace(buf, cursor_ns, 0, -1)
  local row = api.nvim_win_get_cursor(S.code)[1] - 1
  for _, m in ipairs(api.nvim_buf_get_extmarks(buf, marks_ns, { row, 0 }, { row, 0 }, { details = true })) do
    if m[4].hl_group == "OtAdd" then
      api.nvim_buf_set_extmark(buf, cursor_ns, row, 0, { end_row = row + 1, hl_group = "OtAddHere", hl_eol = true, strict = false, priority = 20 })
    end
  end
end


-- A key as you type it: <leader> as yours is, a space as ␣.
local function as_typed(key)
  local leader = vim.g.mapleader or "\\"
  return (key:gsub("<leader>", leader == " " and "␣" or leader))
end

-- A line of ot's key log (cache.rs) for each of the review's keys pressed.
local function pressed(place, k)
  local log = io.open(S.d.keys_path, "a")
  if log then
    log:write(("%d\t%s\t%s\t%s\n"):format(os.time(), place:lower(), k.key, k.act))
    log:close()
  end
end

local function map_keys(buf, place, acts)
  local names = { enter = "<CR>", esc = "<Esc>", tab = "<Tab>", space = "<Space>", left = "<Left>", right = "<Right>" }
  -- A selection's keys work in visual mode.
  local mode = place == "Selection" and "x" or "n"
  for _, k in ipairs(S.d.keys) do
    if vim.tbl_contains(k.on, place) and acts[k.act] then
      local lhs = names[k.key] or k.key:gsub("^ctrl%-(%a)$", "<C-%1>")
      vim.keymap.set(mode, lhs, function()
        pressed(place, k)
        acts[k.act]()
      end, { buffer = buf, nowait = true, silent = true, desc = k.help })
    end
  end
end

-- A buffer made the review's file f, however it was opened: the list's,
-- or one you reached with gd. Read-only, since an edit would keep the
-- checkout from moving on; but on your own MR the real file takes edits,
-- and saving asks to push them.
local function adopt(buf, f)
  vim.bo[buf].modifiable = S.d.mine and vim.bo[buf].buftype == ""
  S.bufs[f] = buf
  map_keys(buf, "Code", code_acts)
  map_keys(buf, "Selection", code_acts)
end

-- File f of the review in the code window: the real file in ot's review
-- checkout once that is at the MR's head, so your LSP works on it; until
-- then, or for a file the MR removed, a read-only copy of its text. A new
-- buffer goes into the window before it loads, so its autocommands (your
-- plugins, your LSP) run there: for a buffer no window shows, nvim runs
-- them in a float over the top of the screen, which any redraw flashes up.
local function show_file(f)
  local buf = S.bufs[f]
  if buf then
    if api.nvim_win_get_buf(S.code) ~= buf then
      api.nvim_win_set_buf(S.code, buf)
    end
    draw()
    return buf
  end
  local file = S.d.files[f]
  if S.root and not file.gone and file.readable then
    buf = vim.fn.bufadd(S.root .. "/" .. file.path)
    -- Ours before it shows: showing it enters it, which adopts a stranger.
    S.bufs[f] = buf
    api.nvim_win_set_buf(S.code, buf)
    adopt(buf, f)
    draw()
    return buf
  end
  buf = api.nvim_create_buf(false, true)
  api.nvim_win_set_buf(S.code, buf)
  vim.bo[buf].buftype = "nofile"
  vim.bo[buf].swapfile = false
  vim.bo[buf].bufhidden = "hide"
  api.nvim_buf_set_name(buf, ("ot://%d/%s"):format(S.d.iid, file.path))
  local text = file.readable and file.text or { "not shown: a side of this file is not text, or too big" }
  api.nvim_buf_set_lines(buf, 0, -1, false, text)
  vim.bo[buf].readonly = true
  local ft = vim.filetype.match({ filename = file.path, buf = buf })
  if ft and file.readable then
    vim.bo[buf].filetype = ft
    -- Nothing is read into this buffer, so a config that loads treesitter
    -- on a file read (LazyVim) may not have it yet: start it here.
    pcall(vim.treesitter.start, buf, vim.treesitter.language.get_lang(ft))
  end
  adopt(buf, f)
  draw()
  return buf
end

-- Lines first..last of the code, with `above` removed lines drawn over
-- first, in the middle of the window when they fit and from near its top
-- when they don't; the cursor on line `on`, else on first.
local function center(first, last, above, on)
  api.nvim_win_call(S.code, function()
    local n = api.nvim_buf_line_count(0)
    first, last = math.max(1, math.min(first, n)), math.max(1, math.min(last, n))
    on = on or first
    local tall, room = last - first + 1 + above, api.nvim_win_get_height(0)
    local top = tall <= room and first - math.floor((room - tall) / 2) or first - 3
    vim.fn.winrestview({ lnum = on, col = 0, topline = math.max(1, top), topfill = top < 1 and above or 0 })
  end)
  bar()
end

-- A row's file in the code, the changed symbols its symbol uses
-- underlined, the view left where it is.
local function light(r)
  S.shown, S.at = r, r.id
  local t = thing(r)
  local buf = show_file(t.file)
  winbar(t.file)
  api.nvim_buf_clear_namespace(buf, here_ns, 0, -1)
  if r.kind == "symbol" and t.lines and as_read(buf, S.d.files[t.file]) then
    for _, m in ipairs(t.mentions) do
      api.nvim_buf_set_extmark(buf, here_ns, m[1] - 1, m[2], { end_col = m[3], hl_group = "OtUses" })
    end
  end
  return t
end

-- A row in the code: a symbol whole, centered, the cursor on its keyword
-- below any doc comment, so ] steps from it into its changes; a file or
-- the changes outside every symbol, at the first.
-- Always from the start: a row shows what it names, however you left it.
-- The bar over a page that is no file (the tour, the change): its name,
-- and its keys when the window has room for them, since nvim cuts a bar
-- that doesn't fit from its left.
local function page_bar(name, place, acts)
  local keys = table.concat(vim.tbl_map(function(piece)
    return piece[1][1] .. piece[2][1]
  end, keys_line(place, acts)), "  ")
  local room = api.nvim_win_get_width(S.code) - W(name) - W(keys) - 4
  vim.wo[S.code].winbar = "%#OtDim# " .. name .. (room >= 0 and ("%=" .. keys .. " ") or "")
end

-- The bar over whatever the code shows, for the width the window has now.
local function bar_again()
  local r = S.shown
  if not r then
    return
  elseif r.kind == "tour" then
    page_bar("the tour", "Tour", tour_acts)
  elseif r.kind == "overview" then
    page_bar("the change", "Code", code_acts)
  else
    winbar(thing(r).file)
  end
end

local function show(r)
  if r.kind == "tour" then
    S.shown, S.at, S.hunk = r, r.id, nil
    api.nvim_win_set_buf(S.code, tour_page())
    vim.wo[S.code].number, vim.wo[S.code].wrap, vim.wo[S.code].linebreak = false, true, true
    -- Only code is long enough to wrap here: it goes on under itself,
    -- clear of the numbers drawn beside it.
    vim.wo[S.code].showbreak = (" "):rep(9)
    page_bar("the tour", "Tour", tour_acts)
    return
  end
  if r.kind == "overview" then
    S.shown, S.at, S.hunk = r, r.id, nil
    api.nvim_win_set_buf(S.code, overview())
    -- Prose, not code: no line numbers, and long lines wrap.
    vim.wo[S.code].number, vim.wo[S.code].wrap, vim.wo[S.code].linebreak, vim.wo[S.code].showbreak = false, true, true, ""
    page_bar("the change", "Code", code_acts)
    api.nvim_win_call(S.code, function()
      vim.fn.winrestview({ topline = 1, lnum = 1 })
    end)
    return
  end
  local t = light(r)
  S.hunk = nil
  if r.kind == "symbol" and t.lines then
    center(t.lines[1], t.lines[2], 0, t.at)
  else
    local h = t.hunks[1] or { at = 1, last = 1, removed = 0 }
    S.hunk = t.hunks[1] and 1
    center(h.at, h.last, h.removed)
  end
end

local function current()
  return S.rows[api.nvim_win_get_cursor(S.list)[1]]
end

-- Seconds as m:ss, or h:mm:ss past an hour.
local function clock(secs)
  local s = math.max(0, math.floor(secs or 0))
  local h, m = math.floor(s / 3600), math.floor(s % 3600 / 60)
  s = s % 60
  return h > 0 and ("%d:%02d:%02d"):format(h, m, s) or ("%d:%02d"):format(m, s)
end

-- The player's lines: what plays, how it stands, where it is, what is
-- next, and the keys.
local function player_draw()
  if not player_open() then
    return
  end
  local m, w = S.music or { title = "" }, api.nvim_win_get_width(S.player)
  local lines, hls = {}, {}
  local function put(segs)
    local s, h = "", {}
    for _, g in ipairs(segs) do
      h[#h + 1] = { #s, #s + #g[1], g[2] }
      s = s .. g[1]
    end
    lines[#lines + 1] = s
    hls[#lines] = h
  end
  if m.title == "" then
    put({ { " nothing playing", "OtDim" } })
  else
    put({ { " " .. m.title, "OtTitle" } })
    put({ { " " .. (m.artist or ""), "OtDim" } })
    local state = m.buffering and "buffering" or m.state
    local facts = { { state, (state == "playing") and "OtGreen" or "OtAmber" } }
    if (m.total or 0) > 1 then
      facts[#facts + 1] = { ("%d of %d"):format(m.index + 1, m.total), "OtDim" }
    end
    facts[#facts + 1] = { ("%+.0f dB"):format(m.volume or 0), "OtDim" }
    if m.shuffle then
      facts[#facts + 1] = { "shuffle", "OtDim" }
    end
    local segs = { { " ", "OtDim" } }
    for i, f in ipairs(facts) do
      if i > 1 then
        segs[#segs + 1] = { "  ·  ", "OtFaint" }
      end
      segs[#segs + 1] = f
    end
    put(segs)
    local done, total = clock(m.position), clock(m.duration)
    local bar = math.max(4, w - 6 - #done - #total)
    local filled = (m.duration or 0) > 0 and math.floor(m.position / m.duration * bar + 0.5) or 0
    put({ { " " .. done .. "  ", "OtDim" }, { ("━"):rep(filled), m.state == "playing" and "OtAmber" or "OtDim" }, { ("─"):rep(bar - filled), "OtFaint" }, { "  " .. total, "OtDim" } })
    if m.next and m.next[1] then
      put({ { " next  ", "OtFaint" }, { m.next[1], "OtDim" } })
    end
  end
  put({ { "", "OtDim" } })
  local foot = { { " ", "OtDim" } }
  for i, piece in ipairs(key_words("Music", player_acts)) do
    if i > 1 then
      foot[#foot + 1] = { "  ", "OtDim" }
    end
    vim.list_extend(foot, piece)
  end
  put(foot)
  set_lines(api.nvim_win_get_buf(S.player), lines, hls)
end

-- The music screen's keys, in the player: each runs cliamp's own command,
-- as the board's does (src/main.rs).
local function cliamp(...)
  local args = { ... }
  return function()
    vim.system(vim.list_extend({ "cliamp" }, args))
  end
end

local function close_player()
  if player_open() then
    api.nvim_win_close(S.player, true)
  end
end

player_acts = {
  PlayPause = cliamp("toggle"),
  NextTrack = cliamp("next"),
  PrevTrack = cliamp("prev"),
  SeekForward = cliamp("seek", "10"),
  SeekBack = cliamp("seek", "-10"),
  Louder = cliamp("remote", "call", "volume.adjust", "--params", '{"value":2}'),
  Softer = cliamp("remote", "call", "volume.adjust", "--params", '{"value":-2}'),
  Shuffle = cliamp("shuffle"),
  Leave = close_player,
  Music = close_player,
}

-- M: the player, a window over the middle, closed by M again or q.
local function player()
  if player_open() then
    close_player()
    return
  end
  local buf = api.nvim_create_buf(false, true)
  vim.bo[buf].bufhidden = "wipe"
  local width = math.max(40, math.min(72, vim.o.columns - 4))
  local win = api.nvim_open_win(buf, true, {
    relative = "editor", width = width, height = 7,
    row = math.floor((vim.o.lines - 9) / 2), col = math.floor((vim.o.columns - width) / 2),
    style = "minimal", border = "rounded", title = " ♪ music ", title_pos = "left",
  })
  S.player = win
  map_keys(buf, "Music", player_acts)
  vim.keymap.set("n", "<Esc>", close_player, { buffer = buf, nowait = true })
  api.nvim_create_autocmd("WinClosed", {
    pattern = tostring(win),
    once = true,
    callback = function()
      S.player = nil
      render()
    end,
  })
  player_draw()
  render()
end

-- The cursor's row in a band, which stays while you are in the code.
local function band(r)
  api.nvim_buf_clear_namespace(S.listbuf, band_ns, 0, -1)
  api.nvim_buf_set_extmark(S.listbuf, band_ns, S.line[r.id] - 1, 0, { line_hl_group = "OtBand" })
end

-- The code follows the list's cursor.
function M.follow()
  local r = current()
  if not resting(r) then
    return
  end
  band(r)
  if not S.shown or S.shown.id ~= r.id then
    show(r)
  end
end

-- The review's file a buffer is, if any: one the list opened, or the same
-- file in the review checkout reached some other way.
local function file_of(buf)
  for f, b in pairs(S.bufs) do
    if b == buf then
      return f
    end
  end
  local name, root = api.nvim_buf_get_name(buf), S.root and (S.root .. "/")
  if not root or name:sub(1, #root) ~= root then
    return nil
  end
  for f, file in ipairs(S.d.files) do
    if file.path == name:sub(#root + 1) and not file.gone then
      return f
    end
  end
end

-- Each window keeps to its own: the list on the left, files on the right.
-- One landing in the other (a buffer line's click, :bnext, ctrl-^) is put
-- where it goes, once the command that brought it is done with it.
function M.keep_to_window()
  if not S.d then
    return
  end
  local win, buf = api.nvim_get_current_win(), api.nvim_get_current_buf()
  if win == S.foot then
    api.nvim_set_current_win(S.list)
  elseif win == S.code and buf == S.listbuf then
    vim.schedule(function()
      show(S.shown or { kind = "overview", id = "overview" })
    end)
  elseif win == S.list and buf ~= S.listbuf and vim.bo[buf].buftype == "" then
    vim.schedule(function()
      api.nvim_win_set_buf(S.list, S.listbuf)
      api.nvim_win_set_buf(S.code, buf)
      api.nvim_set_current_win(S.code)
    end)
  end
end

-- A review file you reached yourself (gd, a search) gets the change drawn
-- on it too, and one you come back to is drawn again, at the width the
-- window has now. The list stays where you left it: only its own keys
-- move it.
function M.adopt_shown()
  if not S.d or api.nvim_get_current_win() ~= S.code then
    return
  end
  local buf = api.nvim_get_current_buf()
  local f = file_of(buf)
  if not f then
    return
  end
  if S.bufs[f] ~= buf then
    adopt(buf, f)
  end
  draw()
end

-- j and k: row to row, over the headings between.
local function step(by)
  for _ = 1, vim.v.count1 do
    local i, n = api.nvim_win_get_cursor(S.list)[1] + by, api.nvim_buf_line_count(S.listbuf)
    while i >= 1 and i <= n and not resting(S.rows[i]) do
      i = i + by
    end
    if resting(S.rows[i]) then
      api.nvim_win_set_cursor(S.list, { i, 0 })
    end
  end
  M.follow()
end

-- ] and [: the shown row's next or previous hunk, staying put at its
-- ends. From a symbol just shown, ] goes to its first.
local function hunk(by)
  local t = S.shown and thing(S.shown)
  if not t or #t.hunks == 0 then
    return
  end
  local to
  if not S.hunk then
    to = by > 0 and 1 or nil
  else
    -- From where the cursor is, if you moved it in the code since.
    local at = api.nvim_win_get_cursor(S.code)[1]
    for i = by > 0 and 1 or #t.hunks, by > 0 and #t.hunks or 1, by do
      local h = t.hunks[i]
      if (by > 0 and h.at > at) or (by < 0 and h.at < at) then
        to = i
        break
      end
    end
  end
  if to then
    S.hunk = to
    local h = t.hunks[to]
    center(h.at, h.last, h.removed)
  end
end

-- The step the cursor is in on the tour's page, if it is past the first.
local function step_here()
  local at, here = api.nvim_win_get_cursor(S.code)[1], nil
  for i, first in ipairs(S.tour_first) do
    if first <= at then
      here = i
    end
  end
  return here
end

-- Something to watch while Claude thinks: a small machine run on a grid of
-- W by H dots, a different one from a different start each time. Each is
-- { name, step(), on(x, y) }, x and y from 0; step moves it one frame on
-- and may change its name (a count in it).
local automata = {
  -- Conway's Life from a random soup, on a torus; a new soup when it
  -- settles.
  function(W, H)
    local g, age, was, still = {}, 0, 0, 0
    local function soup()
      for i = 0, W * H - 1 do
        g[i] = math.random() < 0.3 and 1 or 0
      end
      age, still = 0, 0
    end
    soup()
    local m = { name = "Conway's Life" }
    function m.on(x, y)
      return g[y * W + x] == 1
    end
    function m.step()
      local n, alive = {}, 0
      for y = 0, H - 1 do
        local up, down = ((y - 1) % H) * W, ((y + 1) % H) * W
        for x = 0, W - 1 do
          local l, r = (x - 1) % W, (x + 1) % W
          local c = g[up + l] + g[up + x] + g[up + r] + g[y * W + l] + g[y * W + r] + g[down + l] + g[down + x] + g[down + r]
          local v = (c == 3 or (c == 2 and g[y * W + x] == 1)) and 1 or 0
          n[y * W + x] = v
          alive = alive + v
        end
      end
      g, age = n, age + 1
      still = alive == was and still + 1 or 0
      was = alive
      if still > 30 or age > 600 then
        soup()
      end
    end
    return m
  end,
  -- Brian's Brain: a cell fires when exactly two neighbours fire, then
  -- rests a step. It never settles.
  function(W, H)
    local g = {}
    for i = 0, W * H - 1 do
      g[i] = math.random() < 0.15 and 1 or 0
    end
    local m = { name = "Brian's Brain" }
    function m.on(x, y)
      return g[y * W + x] ~= 0
    end
    function m.step()
      local n = {}
      for y = 0, H - 1 do
        for x = 0, W - 1 do
          local i, c = y * W + x, 0
          if g[i] == 0 then
            for dy = -1, 1 do
              for dx = -1, 1 do
                if g[((y + dy) % H) * W + (x + dx) % W] == 1 then
                  c = c + 1
                end
              end
            end
          end
          n[i] = g[i] == 1 and 2 or (g[i] == 0 and c == 2) and 1 or 0
        end
      end
      g = n
    end
    return m
  end,
  -- One of Wolfram's elementary rules, each row made from the one above,
  -- from a single cell or a random row.
  function(W, H)
    local rules = { 30, 45, 73, 90, 105, 110, 126, 150 }
    local rule = rules[math.random(#rules)]
    local row, rows = {}, {}
    local one = math.random() < 0.5
    for x = 0, W - 1 do
      row[x] = (one and x == math.floor(W / 2) or not one and math.random() < 0.5) and 1 or 0
    end
    local m = { name = "rule " .. rule }
    function m.on(x, y)
      return rows[y + 1] and rows[y + 1][x] == 1
    end
    function m.step()
      rows[#rows + 1] = row
      if #rows > H then
        table.remove(rows, 1)
      end
      local n = {}
      for x = 0, W - 1 do
        local k = row[(x - 1) % W] * 4 + row[x] * 2 + row[(x + 1) % W]
        n[x] = math.floor(rule / 2 ^ k) % 2
      end
      row = n
    end
    return m
  end,
  -- Langton's ant and its cousins: on a cell of colour c the ant turns the
  -- way the rule's c-th letter says, recolours it and steps on.
  function(W, H)
    local rules = { "RL", "RLR", "LLRR", "LRRRRRLLR", "LLRRRLRLRLLR", "RRLLLRLLLRRR", "LRRL" }
    local rule = rules[math.random(#rules)]
    local g, x, y, d, steps = {}, math.floor(W / 2), math.floor(H / 2), math.random(0, 3), 0
    local m = {}
    function m.on(px, py)
      return (g[py * W + px] or 0) ~= 0
    end
    function m.step()
      for _ = 1, 150 do
        local i = y * W + x
        local c = g[i] or 0
        d = (d + (rule:sub(c + 1, c + 1) == "R" and 1 or 3)) % 4
        g[i] = (c + 1) % #rule
        x, y = (x + (d == 1 and 1 or d == 3 and -1 or 0)) % W, (y + (d == 2 and 1 or d == 0 and -1 or 0)) % H
      end
      steps = steps + 150
      m.name = ("Langton's ant, rule %s, step %d"):format(rule, steps)
    end
    m.step()
    return m
  end,
  -- The five-state busy beaver, the Turing machine that runs longest of
  -- its size before it halts (47,176,870 steps): its tape, a row a step.
  function(W, H)
    local machine = { A = { "1RB", "1LC" }, B = { "1RC", "1RB" }, C = { "1RD", "0LE" }, D = { "1LA", "1LD" }, E = { "1RH", "0LA" } }
    local tape, at, state, steps, rows = {}, 0, "A", 0, {}
    local m = {}
    function m.on(x, y)
      local row = rows[y + 1]
      return row and (row[x - math.floor(W / 2)] == 1 or row.at == x - math.floor(W / 2))
    end
    function m.step()
      for _ = 1, 3 do
        local row = { at = at }
        for x = -math.floor(W / 2), math.ceil(W / 2) do
          row[x] = tape[x]
        end
        rows[#rows + 1] = row
        if #rows > H then
          table.remove(rows, 1)
        end
        local to = machine[state][(tape[at] or 0) + 1]
        tape[at] = tonumber(to:sub(1, 1))
        at = at + (to:sub(2, 2) == "R" and 1 or -1)
        state, steps = to:sub(3, 3), steps + 1
      end
      m.name = ("the five-state busy beaver, step %d of 47,176,870"):format(steps)
    end
    m.step()
    return m
  end,
}

-- A machine's grid as rows of braille, eight of its dots to a character.
local function braille(m, cols, rows)
  local dots = { { 1, 8 }, { 2, 16 }, { 4, 32 }, { 64, 128 } }
  local out = {}
  for r = 0, rows - 1 do
    local line = {}
    for c = 0, cols - 1 do
      local bits = 0
      for dy = 0, 3 do
        for dx = 0, 1 do
          if m.on(c * 2 + dx, r * 4 + dy) then
            bits = bits + dots[dy + 1][dx + 1]
          end
        end
      end
      line[#line + 1] = string.char(0xE2, 0xA0 + math.floor(bits / 64), 0x80 + bits % 64)
    end
    out[#out + 1] = { { table.concat(line), "OtTeal" } }
  end
  return out
end

-- The machine over the page's line `line` while Claude reads, as big as
-- the window has room for, a frame every 90ms while the page shows; none
-- once `line` is nil. Drawn as
-- lines of its own over that line, so the page's text never changes under
-- your markdown renderer.
local wait_ns = api.nvim_create_namespace("ot.wait")
local function waiting(buf, line)
  if not line then
    if S.wait then
      S.wait.timer:stop()
      S.wait.timer:close()
      S.wait = nil
      api.nvim_buf_clear_namespace(buf, wait_ns, 0, -1)
    end
    return
  end
  -- The window less its margin, and less the page's title and the two
  -- lines said under the machine. In a window resized since, the same
  -- kind of machine starts again at its new size.
  local info = vim.fn.getwininfo(S.code)[1]
  local cols, rows = math.max(20, info.width - info.textoff - 2), math.max(6, info.height - 6)
  if not S.wait then
    math.randomseed(uv.hrtime() % 2 ^ 31)
    S.wait = { timer = uv.new_timer(), kind = math.random(#automata) }
    S.wait.timer:start(0, 90, vim.schedule_wrap(function()
      local w = S.wait
      if not w or not api.nvim_buf_is_valid(buf) or api.nvim_win_get_buf(S.code) ~= buf then
        return
      end
      w.machine.step()
      local frame = braille(w.machine, w.cols, w.rows)
      frame[#frame + 1] = { { w.machine.name, "OtFaint" } }
      frame[#frame + 1] = { { "" } }
      api.nvim_buf_set_extmark(buf, wait_ns, math.min(w.line, api.nvim_buf_line_count(buf)) - 1, 0, { id = 1, virt_lines = frame, virt_lines_above = true })
    end))
  end
  if S.wait.cols ~= cols or S.wait.rows ~= rows then
    S.wait.machine, S.wait.cols, S.wait.rows = automata[S.wait.kind](cols * 2, rows * 4), cols, rows
  end
  S.wait.line = line
end

-- The tour's page: what the change does, the situations that end
-- differently, then each step over the code it is about, read from top to
-- bottom. It is markdown, so your renderer draws its headings and its
-- names from the code, and each step's code is a fenced block in the
-- file's language, colored as code; the line numbers and the + and - are
-- drawn beside it, not in it. While Claude is at it, what it has written
-- so far, over a machine to watch. Written again whenever more lands,
-- your place kept.
local tour_ns = api.nvim_create_namespace("ot.tour")
tour_page = function()
  local buf, t = S.tourbuf, S.tour
  if not buf or not api.nvim_buf_is_valid(buf) then
    buf = api.nvim_create_buf(false, true)
    vim.bo[buf].buftype, vim.bo[buf].bufhidden, vim.bo[buf].swapfile = "nofile", "hide", false
    api.nvim_buf_set_name(buf, ("ot://!%d/the tour"):format(S.d.iid))
    S.tourbuf = buf
    prose(buf)
    map_keys(buf, "Tour", tour_acts)
    map_keys(buf, "Selection", { ChatAdd = function()
      code_acts.ChatAdd()
    end })
  end
  local info = vim.fn.getwininfo(S.code)[1]
  local w = math.max(20, math.min(84, info.width - info.textoff - 2))
  -- A line is its text, the colors on parts of it, what is drawn before it
  -- (a code line's number and mark) and the band behind it.
  local lines, marks, first = {}, {}, {}
  local function put(text, m)
    lines[#lines + 1] = text
    marks[#lines] = m
  end
  local function para(text)
    for _, l in ipairs(wrap(text, w)) do
      put(l)
    end
  end
  put("# " .. S.d.title)
  put("")
  if t.error then
    put("No tour. " .. t.error, { hl = "OtAmber" })
  end
  if (t.summary or "") ~= "" then
    para(t.summary)
    put("")
  end
  if #(t.cases or {}) > 0 then
    put("## What ends differently")
    for _, c in ipairs(t.cases) do
      put("")
      for _, l in ipairs(wrap(c.when, w)) do
        put(l, { hl = "OtAmber" })
      end
      for _, side in ipairs({ { "before", c.before }, { "now", c.now } }) do
        for j, l in ipairs(wrap(side[2], w - 10)) do
          put("  " .. l, { lead = { { j == 1 and ("%-8s"):format(side[1]) or "        ", "OtFaint" } }, hl = side[1] == "before" and "OtDim" or nil })
        end
      end
    end
    put("")
  end
  local dir = shared_dir()
  -- A step of the walk or a design flag: its title, where it is, what
  -- Claude says, then its code. What to check, or what to do instead,
  -- stands apart, where the eye lands after the words.
  local stops = {}
  local function stop(s, head, apart)
    stops[#stops + 1] = s
    first[#stops] = #lines + 1
    put(head)
    put(("%s:%d"):format(s.path:sub(#dir + 1), s.line), { hl = "OtFaint" })
    put("")
    para(s.says)
    if apart then
      put("")
      for j, l in ipairs(wrap(apart[2], w - 2)) do
        put(l, { lead = { { j == 1 and apart[1] or "  ", apart[3] } }, hl = apart[3] })
      end
    end
    put("")
    put("```" .. (vim.filetype.match({ filename = s.path }) or ""))
    -- The change is what is lit: the lines round it fade, the words a line
    -- changed are brighter on its band, and ▶ is the line it is about.
    for _, l in ipairs(s.code) do
      put(l.text, {
        lead = {
          { l.n == s.line and "▶" or " ", "OtTeal" },
          { l.n and ("%5d "):format(l.n) or "      ", l.n == s.line and "OtTeal" or "OtFaint" },
          { l.mark .. " ", l.mark == "+" and "OtGreen" or l.mark == "-" and "OtRed" or "OtFaint" },
        },
        band = l.mark == "+" and "OtAdd" or l.mark == "-" and "OtDelBand" or nil,
        hl = l.mark == " " and "OtDim" or nil,
        words = l.words and l.words[2] > l.words[1] and { l.words[1], l.words[2], l.mark == "+" and "OtAddWord" or "OtDelWord" } or nil,
      })
    end
    put("```")
    if (s.beyond or 0) > 0 then
      put(("%d more changed %s here. enter reads them in the file."):format(s.beyond, s.beyond == 1 and "line" or "lines"), { hl = "OtFaint" })
    end
    put("")
  end
  -- What Claude flagged in the design is said whole before the walk, what
  -- is wrong and what to do instead, so you read the code knowing it. The
  -- code that proves it comes after the walk.
  if #(t.flags or {}) > 0 then
    put("## Design flags")
    for _, f in ipairs(t.flags) do
      put("")
      put("⚑ " .. f.title, { hl = "OtRed" })
      para(f.says)
      if f.instead ~= "" then
        for j, l in ipairs(wrap(f.instead, w - 2)) do
          put(l, { lead = { { j == 1 and "→ " or "  ", "OtGreen" } }, hl = "OtGreen" })
        end
      end
    end
    put("")
  end
  for i, s in ipairs(t.steps or {}) do
    stop(s, ("## %d  %s"):format(i, (s.title or "") ~= "" and s.title or s.path:match("[^/]*$")), (s.ask or "") ~= "" and { "? ", s.ask, "OtAmber" } or nil)
  end
  for _, f in ipairs(t.flags or {}) do
    stop(f, "## ⚑  " .. f.title, f.instead ~= "" and { "→ ", f.instead, "OtGreen" } or nil)
  end
  local wait_at
  if t.reading then
    wait_at = #lines + 1
    for _, l in ipairs(wrap(#(t.steps or {}) > 0 and "Claude is writing the next step…" or "Claude is reading the change. A minute or two of thought, then it writes.", w)) do
      put(l, { hl = "OtTeal" })
    end
  elseif t.steps then
    put("That is the tour. The list has every symbol, and what you have viewed is still yours to mark.", { hl = "OtDim" })
  end
  local view = api.nvim_win_get_buf(S.code) == buf and api.nvim_win_call(S.code, vim.fn.winsaveview)
  -- Only from the first line that changed, so what your renderer drew on
  -- the lines above stays as Claude writes on below them.
  local old, top = api.nvim_buf_get_lines(buf, 0, -1, false), 0
  while top < #old and top < #lines and old[top + 1] == lines[top + 1] do
    top = top + 1
  end
  vim.bo[buf].modifiable = true
  api.nvim_buf_set_lines(buf, top, -1, false, vim.list_slice(lines, top + 1))
  vim.bo[buf].modifiable = false
  api.nvim_buf_clear_namespace(buf, tour_ns, 0, -1)
  for l, m in pairs(marks) do
    if m.hl then
      api.nvim_buf_set_extmark(buf, tour_ns, l - 1, 0, { end_col = #lines[l], hl_group = m.hl, priority = 300 })
    end
    if m.words then
      api.nvim_buf_set_extmark(buf, tour_ns, l - 1, m.words[1], { end_col = math.min(m.words[2], #lines[l]), hl_group = m.words[3] == "OtDelWord" and "OtDelWordBand" or m.words[3], priority = 5100 })
    end
    if m.lead then
      api.nvim_buf_set_extmark(buf, tour_ns, l - 1, 0, { virt_text = m.lead, virt_text_pos = "inline", right_gravity = false })
    end
    if m.band then
      -- Over the background your markdown renderer gives a code block,
      -- and only a background, so the code's colors show on it.
      api.nvim_buf_set_extmark(buf, tour_ns, l - 1, 0, { end_row = l, hl_group = m.band, hl_eol = true, strict = false, priority = 5000 })
    end
  end
  -- nvim tells plugins of a change only in the current buffer, and the
  -- list has the cursor while this is written.
  api.nvim_exec_autocmds("TextChanged", { buffer = buf, modeline = false })
  if view then
    api.nvim_win_call(S.code, function()
      vim.fn.winrestview(view)
    end)
  end
  S.tour_first, S.tour_stops = first, stops
  waiting(buf, wait_at)
  return buf
end

-- n and p: the tour's page in the code at its next or previous step,
-- counted from where you are reading on it, or from the step you went into
-- the code from. The list goes to the tour's row.
local function tour(by)
  if not S.tour then
    S.say = { "no tour. T on the board opens the review on one", "OtAmber" }
    render()
    return
  end
  local on_page = api.nvim_win_get_buf(S.code) == S.tourbuf
  local to = math.max(1, math.min(#S.tour_first, ((on_page and step_here() or not on_page and S.step) or 0) + by))
  S.step = nil
  show({ kind = "tour", id = "tour" })
  if S.tour_first[to] then
    api.nvim_win_call(S.code, function()
      vim.fn.winrestview({ topline = math.max(1, S.tour_first[to] - 1), lnum = S.tour_first[to], col = 0 })
    end)
  end
  render()
end

-- enter on the tour's page: the file of the step you are reading, at its
-- line, under what Claude says of it. Shown under the tour's own row, so
-- the list stays there and ] walks that file's hunks from the step.
local function into_step()
  local i = step_here()
  if not i then
    return
  end
  local s = S.tour_stops[i]
  S.step = i
  S.shown, S.hunk = { kind = "file", id = "tour", i = s.file }, 1
  local buf = show_file(s.file)
  winbar(s.file)
  api.nvim_buf_clear_namespace(buf, here_ns, 0, -1)
  local info = vim.fn.getwininfo(S.code)[1]
  center(s.line, s.line, #step_rows(i, info.width - info.textoff), s.line)
end

local function open()
  local r = current()
  if not r or r.kind ~= "symbol" or #thing(r).kids == 0 then
    return
  end
  if S.open[r.key] then
    step(1)
  else
    S.open[r.key] = true
    render()
  end
end

local function close()
  local r = current()
  if not r or r.kind ~= "symbol" then
    return
  end
  if S.open[r.key] then
    S.open[r.key] = nil
  elseif r.parent then
    S.open[r.parent] = nil
    S.at = "s:" .. r.parent
  end
  render()
end

-- tab: the other list, starting on the file the code shows.
-- tab: the other list, where you left it; the first time, on the file the
-- code shows.
local function other_list()
  local f = S.shown and S.shown.i and thing(S.shown).file
  S.tabs[S.tab] = { at = S.at, top = api.nvim_win_call(S.list, vim.fn.winsaveview).topline }
  S.tab = S.tab == "symbols" and "files" or "symbols"
  local was = S.tabs[S.tab]
  S.at = was and was.at
  if not was then
    for _, r in ipairs(rows()) do
      if not S.at and resting(r) and thing(r).file == f then
        S.at = r.id
      end
    end
  end
  render()
  if was then
    api.nvim_win_call(S.list, function()
      vim.fn.winrestview({ topline = was.top, lnum = api.nvim_win_get_cursor(0)[1], col = 0 })
    end)
  end
end

local function aside()
  S.aside = not S.aside
  render()
end

-- v: the mark kept in ot's file of them, which other reviews read too.
-- } and {: the next or previous comment in the whole change, threads and your
-- drafts, in file and line order, round from the last to the first.
local function spots()
  local out = {}
  local function at(f, line, old)
    if line then
      return line
    end
    for _, b in ipairs(S.d.files[f] and S.d.files[f].removed or {}) do
      for _, l in ipairs(b.lines) do
        if l.old == old then
          return b.at
        end
      end
    end
  end
  for _, t in ipairs(S.threads) do
    local line = t.file and at(t.file, t.line, t.old)
    if line then
      local who = t.notes[1] and t.notes[1].author or ""
      local body = t.notes[1] and t.notes[1].body or ""
      table.insert(out, { file = t.file, line = line, what = (t.resolved and "resolved · " or "") .. who .. ": " .. body })
    end
  end
  for _, d in ipairs(S.drafts) do
    local line = d.file and not d.reply_to and at(d.file, d.line, d.old)
    if line then
      table.insert(out, { file = d.file, line = line, what = "your draft: " .. d.body })
    end
  end
  table.sort(out, function(a, b)
    return a.file < b.file or (a.file == b.file and a.line < b.line)
  end)
  return out
end

local function comment_jump(by)
  local all = spots()
  if #all == 0 then
    S.say = { "no comments on this change", "OtDim" }
    return render()
  end
  local f, line = 0, 0
  if S.shown and S.shown.kind ~= "overview" and S.shown.kind ~= "tour" then
    f = S.shown.kind == "file" and S.shown.i or thing(S.shown).file
    line = api.nvim_win_get_cursor(S.code)[1]
  end
  local to
  for i = by > 0 and 1 or #all, by > 0 and #all or 1, by do
    local s = all[i]
    local after = s.file > f or (s.file == f and s.line > line)
    local before = s.file < f or (s.file == f and s.line < line)
    if (by > 0 and after) or (by < 0 and before) then
      to = i
      break
    end
  end
  to = to or (by > 0 and 1 or #all)
  local s = all[to]
  -- The list's row it is in, the tightest symbol round it, so the code follows that row.
  local row, span
  for _, r in ipairs(S.rows) do
    if resting(r) and r.kind ~= "overview" and r.kind ~= "tour" then
      local t = thing(r)
      local file = r.kind == "file" and r.i or t.file
      if file == s.file then
        local lines = r.kind == "symbol" and t.lines
        local inside = not lines or (lines[1] <= s.line and s.line <= lines[2])
        local size = lines and (lines[2] - lines[1]) or math.huge
        if inside and (not span or size < span) then
          row, span = r, size
        end
      end
    end
  end
  local what = s.what:gsub("%s+", " ")
  S.say = { ("comment %d of %d · %s"):format(to, #all, what), "OtOrange" }
  if row then
    S.at = row.id
    render()
  else
    S.shown, S.hunk = { kind = "file", id = (current() or {}).id, i = s.file }, nil
    local buf = show_file(s.file)
    winbar(s.file)
    api.nvim_buf_clear_namespace(buf, here_ns, 0, -1)
    render()
  end
  center(s.line, s.line, 0, s.line)
end

local function viewed()
  local r = current()
  if not resting(r) or r.kind == "overview" or r.kind == "tour" then
    return
  end
  local path = S.d.viewed_path
  local now = (not S.viewed[thing(r).mark]) or nil
  -- The row and everything under it take its new state: a symbol's kids, all
  -- the way down, and a file's symbols and changes outside one.
  local marks, seen = {}, {}
  local function under(i)
    if seen[i] then
      return
    end
    seen[i] = true
    marks[#marks + 1] = S.d.symbols[i].mark
    for _, k in ipairs(S.d.symbols[i].kids) do
      under(k)
    end
  end
  if r.kind == "symbol" then
    under(r.i)
  else
    marks[1] = thing(r).mark
  end
  if r.kind == "file" then
    for i, sym in ipairs(S.d.symbols) do
      if sym.file == r.i then
        under(i)
      end
    end
    for _, l in ipairs(S.d.loose) do
      if l.file == r.i then
        marks[#marks + 1] = l.mark
      end
    end
  end
  for _, m in ipairs(marks) do
    S.viewed[m] = now
  end
  local all = {}
  local f = io.open(path, "r")
  if f then
    for l in f:lines() do
      all[l] = l ~= "" or nil
    end
    f:close()
  end
  for _, m in ipairs(marks) do
    all[m] = now
  end
  local keep = vim.tbl_keys(all)
  table.sort(keep)
  vim.fn.mkdir(vim.fs.dirname(path), "p")
  local tmp = path .. ".nvim"
  local w = io.open(tmp, "w")
  if w then
    w:write(#keep > 0 and (table.concat(keep, "\n") .. "\n") or "")
    w:close()
    uv.fs_rename(tmp, path)
  end
  render()
end

-- Scroll keys from the list scroll the code. ctrl-d and ctrl-u go through
-- neoscroll if your config has it, told which window, since replaying your
-- key in the code would animate the list instead; ctrl-e and ctrl-y a line.
local smooth = { ["<C-d>"] = "ctrl_d", ["<C-u>"] = "ctrl_u" }
local function scroll(key)
  return function()
    local ok, neoscroll = pcall(require, "neoscroll")
    if ok and smooth[key] then
      return neoscroll[smooth[key]]({ duration = 250, winid = S.code })
    end
    api.nvim_win_call(S.code, function()
      vim.cmd.normal({ api.nvim_replace_termcodes(key, true, false, true), bang = true })
    end)
  end
end

-- ?: the keys, in the list's window, until any of ? q esc.
local function help()
  local buf = api.nvim_create_buf(false, true)
  local lines, seen = {}, {}
  for _, k in ipairs(S.d.keys) do
    local line = ("%-8s %s"):format(as_typed(k.key), k.help)
    if not seen[line] then
      seen[line] = true
      lines[#lines + 1] = line
    end
  end
  api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modifiable = false
  vim.bo[buf].bufhidden = "wipe"
  local view = api.nvim_win_call(S.list, vim.fn.winsaveview)
  api.nvim_win_set_buf(S.list, buf)
  for _, k in ipairs({ "?", "q", "<Esc>" }) do
    vim.keymap.set("n", k, function()
      api.nvim_win_set_buf(S.list, S.listbuf)
      api.nvim_win_call(S.list, function()
        vim.fn.winrestview(view)
      end)
    end, { buffer = buf, nowait = true })
  end
end

-- Back to the board, past your config's quit hooks: a session saved now
-- would be this review's buffers in place of your own. Without the bang,
-- so an edit made in a file you jumped to is named, not thrown away.
local function leave()
  vim.cmd("noautocmd qa")
end

-- A buffer of words, not code: markdown, which your renderer draws as
-- GitLab would, and nothing completes them or marks them wrong (coc's
-- switches, and the diagnostics any server would leave).
prose = function(buf)
  vim.b[buf].coc_enabled = 0
  vim.b[buf].coc_suggest_disable = 1
  vim.b[buf].coc_diagnostic_disable = 1
  vim.diagnostic.enable(false, { bufnr = buf })
  vim.bo[buf].filetype = "markdown"
end

-- One request to ot over its socket, a JSON line each way: a comment,
-- finishing the review, a picture a body shows. got gets the answer, or
-- nil when ot didn't give one.
local function call(req, got)
  local pipe = uv.new_pipe(false)
  local read = ""
  local function done(answer)
    vim.schedule(function()
      if not pipe:is_closing() then
        pipe:close()
      end
      got(answer)
    end)
  end
  pipe:connect(vim.env.OT_SOCK or "", function(err)
    if err then
      return done({ error = "ot isn't listening: " .. err })
    end
    pipe:read_start(function(rerr, chunk)
      if rerr or not chunk then
        return done(nil)
      end
      read = read .. chunk
      if read:find("\n") then
        pipe:read_stop()
        local ok, v = pcall(vim.json.decode, read, { luanil = { object = true, array = true } })
        done(ok and v or nil)
      end
    end)
    pipe:write(vim.json.encode(req) .. "\n")
  end)
end

-- A line of a body with its pictures and videos as ot's copies of them
-- (src/nvim.rs upload), which your image plugin draws under the line; a
-- video as its first frame, under a link gx plays. One not here yet is
-- asked of ot once and drawn when it lands. GitLab's {width=… height=…}
-- after an image only sizes it.
local function media(l)
  l = l:gsub("(!%b[]%b())%b{}", "%1")
  return (l:gsub("!(%b[])%((/uploads/[^)%s]+)%)", function(alt, path)
    local got = S.uploads[path]
    if got == nil then
      S.uploads[path] = false
      call({ op = "upload", path = path }, function(a)
        if a and not a.error then
          S.uploads[path] = a
          overview()
        end
      end)
    end
    if not got then
      return nil
    end
    if got.still then
      return ("[▶ %s](%s) ![](%s)"):format(alt:sub(2, -2), got.file, got.still)
    end
    return ("!%s(%s)"):format(alt, got.file)
  end))
end

-- What GitLab shows of a body, line by line. A <details> block is shut:
-- one line of its summary (GitLab's bots keep every stale copy of their
-- comment in one, nested in the last), since no renderer folds HTML. An
-- HTML link (their summaries have them) is a markdown one.
local function said(body)
  local out, depth, summary = {}, 0, nil
  for _, l in ipairs(vim.split((body:gsub("\r", "")), "\n", { plain = true })) do
    l = l:gsub('<a href="([^"]*)"[^>]*>(.-)</a>', "[%2](%1)")
    local rest, opened = l:gsub("<details[^>]*>", "")
    local text, closed = rest:gsub("</details>", "")
    if depth == 0 and opened == 0 then
      out[#out + 1] = media(l)
    else
      summary = summary or text:match("<summary>(.-)</summary>")
      local before = depth == 0 and l:match("^(.-)%s*<details") or ""
      if before ~= "" then
        out[#out + 1] = before
      end
      depth = depth + opened - closed
      if depth <= 0 then
        out[#out + 1] = "▸ " .. (summary or "details")
        depth, summary = 0, nil
      end
    end
  end
  -- A block still open at the end (your own draft, mid-sentence) shows
  -- what it has so far.
  if depth > 0 then
    out[#out + 1] = "▸ " .. (summary or "details")
  end
  return out
end

-- "The change": the MR's description, its pipeline, and what was said on
-- it rather than on a line, in a buffer of its own. A comment is a quote,
-- so its markdown still reads as markdown under the bar. Each comment
-- line says which thread or draft it is, for r, x and c there.
overview = function()
  local d = S.d
  local buf = S.overview
  if not buf or not api.nvim_buf_is_valid(buf) then
    buf = api.nvim_create_buf(false, true)
    vim.bo[buf].buftype = "nofile"
    vim.bo[buf].bufhidden = "hide"
    api.nvim_buf_set_name(buf, ("ot://!%d/the change"):format(d.iid))
    S.overview = buf
    prose(buf)
    -- snacks draws pictures in the markdown it finds once your first file
    -- read sets it up; the change is no file, and comes first.
    pcall(function()
      Snacks.image.doc.attach(buf)
    end)
    map_keys(buf, "Code", code_acts)
  end
  local lines, hls, at = {}, {}, {}
  local function put(text, hl, what)
    lines[#lines + 1] = text
    hls[#lines] = hl and { { 0, #text, hl } } or {}
    at[#lines] = what
  end
  -- A comment is a quote: who said it, then what, as GitLab shows it. A
  -- bare > on a blank line keeps the quote, and its bar, in one piece. A
  -- name's underscores are its own, not italics (GitLab's bots have them).
  local function quote(head, hl, body, what)
    put("> " .. (head:gsub("_", "\\_")), hl, what)
    for _, l in ipairs(said(body)) do
      put(l == "" and ">" or ("> " .. l), nil, what)
    end
  end
  put(d.name .. "  " .. d.title, "OtTitle")
  local pipe = d.pipeline == "failed" and #d.failed_jobs > 0 and (table.concat(d.failed_jobs, ", ") .. " failed") or ("pipeline " .. (d.pipeline ~= "" and d.pipeline or "none"))
  put(("%s  ·  %s  ·  %s"):format(d.author, d.size, pipe), "OtDim")
  put("")
  for _, l in ipairs(said(S.description ~= "" and S.description or "(no description)")) do
    put(l)
  end
  local general = vim.tbl_filter(function(t)
    return not t.file
  end, S.threads)
  local mine = vim.tbl_filter(function(x)
    return not x.file and not x.reply_to
  end, S.drafts)
  put("")
  put(("comments  %d"):format(#general + #mine), "OtDim")
  for _, t in ipairs(general) do
    put("")
    if t.resolved then
      put(("> resolved: %s"):format((t.notes[1] and t.notes[1].author or ""):gsub("_", "\\_")), "OtFaint", { thread = t })
    else
      for _, n in ipairs(t.notes) do
        quote(n.author, "OtOrange", n.body, { thread = t })
      end
    end
    for _, r in ipairs(S.drafts) do
      if r.reply_to == t.id then
        quote("your draft, not sent yet", "OtPink", r.body, { draft = r })
      end
    end
  end
  for _, r in ipairs(mine) do
    put("")
    quote("your draft, not sent yet", "OtPink", r.body, { draft = r })
  end
  -- Only the lines between the first and last that changed are written:
  -- what your plugins hang on a line (a picture, the renderer's marks)
  -- stays on it, where a whole rewrite slides it all past the end.
  local old = api.nvim_buf_get_lines(buf, 0, -1, false)
  local top, bottom = 0, 0
  while top < #old and top < #lines and old[top + 1] == lines[top + 1] do
    top = top + 1
  end
  while bottom < #old - top and bottom < #lines - top and old[#old - bottom] == lines[#lines - bottom] do
    bottom = bottom + 1
  end
  if #old ~= #lines or top < #old then
    vim.bo[buf].modifiable = true
    api.nvim_buf_set_lines(buf, top, #old - bottom, false, vim.list_slice(lines, top + 1, #lines - bottom))
    vim.bo[buf].modifiable = false
    -- nvim says the text changed only of the buffer you are in, and this
    -- one is mostly redrawn from the list: unheard, your renderer keeps
    -- drawing the old text's marks, and the markdown looks gone.
    api.nvim_exec_autocmds("TextChanged", { buffer = buf, modeline = false })
  end
  api.nvim_buf_clear_namespace(buf, marks_ns, 0, -1)
  for l, hs in pairs(hls) do
    for _, h in ipairs(hs) do
      api.nvim_buf_set_extmark(buf, marks_ns, l - 1, h[1], { end_col = h[2], hl_group = h[3] })
    end
  end
  S.overview_at = at
  return buf
end

-- Asks ot to do something on GitLab (a comment, finishing the review) and
-- shows what came of it: `said` once it went, else what went wrong.
local function send(req, said)
  S.say = { "sending…", "OtGreen" }
  render()
  call(req, function(answer)
    if not answer or answer.error then
      local why = (answer and answer.error) or "ot didn't answer"
      S.say = { why, "OtAmber" }
      render()
      -- Said loudly too, and kept in :messages, since this one didn't go.
      vim.notify(("%s didn't go: %s"):format(said, why), vim.log.levels.ERROR)
      return
    end
    S.say = { said, "OtGreen" }
    M.talk(answer)
  end)
end

-- A box to write in, under the cursor's line or at the foot of the screen:
-- :w sends what it holds and closes it, q drops it. Tab runs o.tab, for the
-- finishing box's choice.
local function box(o)
  local buf = api.nvim_create_buf(false, true)
  vim.bo[buf].buftype = "acwrite"
  vim.bo[buf].bufhidden = "wipe"
  api.nvim_buf_set_name(buf, ("ot://box/%d"):format(buf))
  api.nvim_buf_set_lines(buf, 0, -1, false, o.text or { "" })
  prose(buf)
  vim.bo[buf].modified = false
  local width = math.max(40, math.min(90, api.nvim_win_get_width(S.code) - 6))
  local cfg = o.foot and { relative = "editor", row = vim.o.lines - 12, col = math.floor((vim.o.columns - width) / 2), width = width, height = 6 }
    or { relative = "cursor", row = 1, col = 0, width = width, height = 5 }
  cfg.style, cfg.border, cfg.title, cfg.title_pos = "minimal", "rounded", " " .. o.title() .. " ", "left"
  local win = api.nvim_open_win(buf, true, cfg)
  vim.wo[win].wrap, vim.wo[win].linebreak = true, true
  local function close()
    if api.nvim_win_is_valid(win) then
      api.nvim_win_close(win, true)
    end
  end
  api.nvim_create_autocmd("BufWriteCmd", {
    buffer = buf,
    callback = function()
      local text = vim.trim(table.concat(api.nvim_buf_get_lines(buf, 0, -1, false), "\n"))
      vim.bo[buf].modified = false
      close()
      o.send(text)
    end,
  })
  vim.keymap.set("n", "q", close, { buffer = buf, nowait = true })
  if o.tab then
    vim.keymap.set("n", "<Tab>", function()
      o.tab()
      api.nvim_win_set_config(win, { title = " " .. o.title() .. " ", title_pos = "left" })
    end, { buffer = buf, nowait = true })
  end
  if not o.text then
    vim.cmd.startinsert()
  end
end

-- Where the code's cursor is, as the review names it: a file and its line,
-- or the overview and what its line is.
local function spot()
  local buf, line = api.nvim_get_current_buf(), api.nvim_win_get_cursor(0)[1]
  if buf == S.overview then
    return { overview = true, what = S.overview_at[line] or {} }
  end
  local f = file_of(buf)
  return f and { file = f, line = line } or nil
end

local function draft_at(here)
  if here.overview then
    return here.what.draft
  end
  for _, d in ipairs(S.drafts) do
    if d.file == here.file and d.line == here.line and not d.reply_to then
      return d
    end
  end
end

local function thread_at(here)
  if here.overview then
    return here.what.thread
  end
  local gone = {}
  for _, b in ipairs(S.d.files[here.file].removed) do
    if b.at == here.line then
      for _, l in ipairs(b.lines) do
        gone[l.old] = true
      end
    end
  end
  for _, t in ipairs(S.threads) do
    if t.file == here.file and ((t.line and t.line == here.line) or (not t.line and gone[t.old])) then
      return t
    end
  end
end

local function sends()
  return S.d.mine and "sends it" or "saves a draft"
end

-- c: a comment on the line, or on the lines selected (on the MR, in the
-- overview); on your draft there, the draft again to change, or to empty
-- and so delete.
local function comment()
  -- Outside visual mode, "v" is the cursor's line too.
  local v, cursor = vim.fn.line("v"), vim.fn.line(".")
  local first, last = math.min(v, cursor), math.max(v, cursor)
  if api.nvim_get_mode().mode:find("^[vV\22]") then
    vim.cmd.normal({ api.nvim_replace_termcodes("<Esc>", true, false, true), bang = true })
  end
  local here = spot()
  if not here then
    return
  end
  local d = first == last and draft_at(here)
  if d then
    return box({
      title = function()
        return "your draft · :w saves it, empty deletes it · q leaves it"
      end,
      text = vim.split(d.body, "\n", { plain = true }),
      send = function(body)
        send({ op = "edit", id = d.id, body = body }, body == "" and "draft deleted" or "draft saved")
      end,
    })
  end
  local where = here.overview and "the MR" or (first == last and ("line " .. last) or ("lines %d to %d"):format(first, last))
  box({
    title = function()
      return ("comment on %s · :w %s · q drops it"):format(where, sends())
    end,
    send = function(body)
      local from = first < last and first or nil
      send({ op = "comment", file = here.file, line = here.file and last, from = from, body = body }, S.d.mine and "sent" or "draft saved")
    end,
  })
end

-- C: a comment on the removed lines drawn just above the cursor's line.
local function comment_gone()
  local here = spot()
  local block
  for _, b in ipairs(here and here.file and S.d.files[here.file].removed or {}) do
    if b.at == here.line then
      block = b
    end
  end
  if not block then
    S.say = { "no removed lines just above this one", "OtGreen" }
    return render()
  end
  local old = block.lines[#block.lines].old
  box({
    title = function()
      return ("comment on removed line %d · :w %s · q drops it"):format(old, sends())
    end,
    send = function(body)
      send({ op = "comment", file = here.file, old = old, body = body }, S.d.mine and "sent" or "draft saved")
    end,
  })
end

-- r: a reply under the thread on this line.
local function reply()
  local here = spot()
  local t = here and thread_at(here)
  if not t then
    S.say = { "no thread on this line", "OtGreen" }
    return render()
  end
  box({
    title = function()
      return ("reply to %s · :w %s · q drops it"):format(t.notes[1] and t.notes[1].author or "the thread", sends())
    end,
    send = function(body)
      send({ op = "comment", reply = t.id, body = body }, S.d.mine and "sent" or "draft saved")
    end,
  })
end

-- x: the thread on this line resolved, or open again.
local function resolve()
  local here = spot()
  local t = here and thread_at(here)
  if not t or not t.resolvable then
    S.say = { t and "a plain comment can't be resolved" or "no thread on this line", "OtGreen" }
    return render()
  end
  send({ op = "resolve", thread = t.id, resolved = not t.resolved }, t.resolved and "reopened" or "resolved")
end

-- A question in a line at the foot of the screen, and the keys that
-- answer it: each closes the line, then does its part. Esc only closes.
local function ask(pieces, on)
  local buf = api.nvim_create_buf(false, true)
  vim.bo[buf].bufhidden = "wipe"
  local text, hls = " ", {}
  for _, p in ipairs(pieces) do
    hls[#hls + 1] = { #text, #text + #p[1], p[2] }
    text = text .. p[1]
  end
  local width = math.min(W(text) + 1, vim.o.columns - 4)
  local win = api.nvim_open_win(buf, true, { relative = "editor", row = vim.o.lines - 6, col = math.floor((vim.o.columns - width) / 2), width = width, height = 1, style = "minimal", border = "rounded" })
  set_lines(buf, { text }, { hls })
  vim.wo[win].cursorline = false
  local function close()
    if api.nvim_win_is_valid(win) then
      api.nvim_win_close(win, true)
    end
  end
  for key, go in pairs(on) do
    vim.keymap.set("n", key, function()
      close()
      go()
    end, { buffer = buf, nowait = true })
  end
  vim.keymap.set("n", "<Esc>", close, { buffer = buf, nowait = true })
  api.nvim_create_autocmd("WinLeave", { buffer = buf, once = true, callback = close })
end

-- A and S: finishing the review, in one box: your drafts go, with a
-- comment if you write one, and an approval unless you tab it away.
-- " with your 2 drafts", for a question about finishing; nothing while
-- there are none.
local function drafts_said()
  local n = #S.drafts
  return n > 0 and (" with your %d %s"):format(n, n == 1 and "draft" or "drafts") or ""
end

local function finish(approve)
  return function()
    local drafts = drafts_said()
    box({
      foot = true,
      title = function()
        return ("%s %s%s · tab: %s · :w sends · q drops it"):format(approve and "approve" or "comment on", S.d.name, drafts, approve and "don't approve" or "approve too")
      end,
      tab = function()
        approve = not approve
      end,
      send = function(body)
        send({ op = "finish", body = body, approve = approve }, approve and "approved" or "sent")
      end,
    })
  end
end

-- A: approving, once you say how: as it is, your drafts going with it, or
-- with a note first.
local function approve()
  ask({
    { ("approve %s%s?  "):format(S.d.name, drafts_said()), "OtAmber" },
    { "y", "OtText" }, { " yes  ", "OtDim" },
    { "m", "OtText" }, { " with a note  ", "OtDim" },
    { "esc", "OtText" }, { " no", "OtDim" },
  }, {
    y = function()
      send({ op = "finish", body = "", approve = true }, "approved")
    end,
    m = finish(true),
  })
end

-- Asks ot to push the checkout's edits, and says what went or why
-- nothing did.
local function push(req)
  S.say = { "pushing…", "OtGreen" }
  render()
  call(req, function(answer)
    S.say = answer and answer.pushed and { answer.pushed, "OtGreen" } or { (answer and answer.error) or "ot didn't answer", "OtAmber" }
    render()
  end)
end

-- A file of the checkout saved, on your own MR: pushed, folded into its
-- last commit or as a new one. All you changed there goes, so a save you
-- said not yet to goes with the next. Asked once the write is done, so
-- :wq just quits.
function M.saved(ev)
  local root = S.root and (S.root .. "/")
  if not (S.d and S.d.mine and root) or api.nvim_buf_get_name(ev.buf):sub(1, #root) ~= root then
    return
  end
  vim.schedule(function()
    ask({
      { ("push your change to %s?  "):format(S.d.name), "OtAmber" },
      { "a", "OtText" }, { " amend its last commit  ", "OtDim" },
      { "c", "OtText" }, { " as a new commit  ", "OtDim" },
      { "esc", "OtText" }, { " not yet", "OtDim" },
    }, {
      a = function()
        push({ op = "amend" })
      end,
      c = function()
        box({
          foot = true,
          title = function()
            return "commit message · :w commits and pushes · q drops it"
          end,
          send = function(message)
            push({ op = "commit", message = message })
          end,
        })
      end,
    })
  end)
end

local function web()
  vim.ui.open(S.d.url)
end

-- y: the symbol as you'd name it to Claude or in Slack.
local function copy()
  local r = current()
  if not r or r.kind ~= "symbol" then
    return
  end
  local s = thing(r)
  local text = ("%s in %s"):format(s.name, S.d.files[s.file].path)
  vim.fn.setreg("+", text)
  S.say = { "copied " .. text, "OtGreen" }
  render()
end

list_acts = {
  Music = player,
  Web = web,
  Copy = copy,
  Down = function()
    step(1)
  end,
  Up = function()
    step(-1)
  end,
  Into = function()
    api.nvim_set_current_win(S.code)
  end,
  Open = open,
  Collapse = close,
  NextHunk = function()
    hunk(1)
  end,
  PrevHunk = function()
    hunk(-1)
  end,
  NextStep = function()
    tour(1)
  end,
  PrevStep = function()
    tour(-1)
  end,
  NextComment = function()
    comment_jump(1)
  end,
  PrevComment = function()
    comment_jump(-1)
  end,
  PageDown = scroll("<C-d>"),
  PageUp = scroll("<C-u>"),
  LineDown = scroll("<C-e>"),
  LineUp = scroll("<C-y>"),
  Tab = other_list,
  Aside = aside,
  Viewed = viewed,
  Help = help,
  Approve = approve,
  Submit = finish(false),
  Leave = leave,
}

-- In the code your own keys move, scroll and change windows; these are
-- the review's.
code_acts = {
  Music = player,
  NextHunk = list_acts.NextHunk,
  PrevHunk = list_acts.PrevHunk,
  NextStep = list_acts.NextStep,
  NextComment = list_acts.NextComment,
  PrevComment = list_acts.PrevComment,
  -- Back to the step you came from.
  PrevStep = function()
    tour(0)
  end,
  Comment = comment,
  CommentGone = comment_gone,
  Reply = reply,
  Resolve = resolve,
  Help = help,
  Leave = leave,
}

-- The panes at the right (the T3 thread, the chat) end as a chat app does:
-- room under the last message, and a box at the foot saying how to reply, or
-- that an answer is on its way.
local PAD = 4
local foot_ns = api.nvim_create_namespace("ot.foot")

local function padded(lines)
  for _ = 1, PAD do
    lines[#lines + 1] = ""
  end
  return lines
end

-- The pane at its end: its last line at the bottom, the cursor on the last message.
local function pane_end(win, buf)
  if not (win and api.nvim_win_is_valid(win) and buf and api.nvim_buf_is_valid(buf)) then
    return
  end
  local total = api.nvim_buf_line_count(buf)
  api.nvim_win_set_cursor(win, { total, 0 })
  api.nvim_win_call(win, function()
    vim.cmd("normal! zb")
  end)
  api.nvim_win_set_cursor(win, { math.max(1, total - PAD), 0 })
end

-- The box at the foot of pane `p` ({ win, foot, footbuf }), saying `text`.
local function pane_foot(p, text, hl)
  p.foot_says = { text, hl }
  if not (p.win and api.nvim_win_is_valid(p.win)) then
    return
  end
  if not (p.footbuf and api.nvim_buf_is_valid(p.footbuf)) then
    p.footbuf = api.nvim_create_buf(false, true)
  end
  local line = " " .. text
  api.nvim_buf_set_lines(p.footbuf, 0, -1, false, { line })
  api.nvim_buf_clear_namespace(p.footbuf, foot_ns, 0, -1)
  api.nvim_buf_set_extmark(p.footbuf, foot_ns, 0, 0, { end_col = #line, hl_group = hl })
  local w, h = api.nvim_win_get_width(p.win), api.nvim_win_get_height(p.win)
  local cfg = { relative = "win", win = p.win, row = math.max(0, h - 3), col = 1, width = math.max(10, w - 4), height = 1, style = "minimal", border = "rounded", focusable = false, zindex = 45 }
  if p.foot and api.nvim_win_is_valid(p.foot) then
    api.nvim_win_set_config(p.foot, cfg)
  else
    p.foot = api.nvim_open_win(p.footbuf, false, cfg)
    vim.wo[p.foot].winhighlight = "NormalFloat:Normal,FloatBorder:OtDim"
  end
end

local function pane_foot_close(p)
  if p.foot and api.nvim_win_is_valid(p.foot) then
    api.nvim_win_close(p.foot, true)
  end
  p.foot = nil
end

local panes = {}
api.nvim_create_autocmd("WinResized", {
  callback = function()
    for _, p in ipairs(panes) do
      if p.foot_says and p.win and api.nvim_win_is_valid(p.win) then
        pane_foot(p, p.foot_says[1], p.foot_says[2])
      end
    end
  end,
})

-- The T3 thread at work on this MR, at the right of the screen as ot's own
-- conversation page: ot runs there (`ot thread`), so it reads, replies (s) and
-- keeps up as the board's page does. ctrl-h goes back to the code.
local thread = { buf = nil, win = nil, job = nil }

local function thread_hide()
  if thread.win and api.nvim_win_is_valid(thread.win) then
    api.nvim_win_close(thread.win, true)
  end
  thread.win = nil
end

local function thread_show(focus)
  if not (S.d and S.d.thread) then
    S.say = { "no T3 thread works on this MR", "OtAmber" }
    render()
    return
  end
  local back = api.nvim_get_current_win()
  if not (thread.win and api.nvim_win_is_valid(thread.win)) then
    -- Over the chat when it is open, so the two share the right of the screen.
    local chat_win = vim.fn.bufwinid(("ot://%s/chat"):format(S.d.name))
    if chat_win ~= -1 then
      api.nvim_set_current_win(chat_win)
      vim.cmd("aboveleft split")
    else
      vim.cmd("botright vsplit")
      api.nvim_win_set_width(0, math.max(56, math.floor(vim.o.columns * 0.36)))
    end
    thread.win = api.nvim_get_current_win()
    vim.wo[thread.win].winbar = "%#OtDim# T3 · " .. S.d.thread.name:gsub("%%", "%%%%") .. "%=ctrl-h back to the code "
    vim.wo[thread.win].number, vim.wo[thread.win].relativenumber = false, false
    vim.wo[thread.win].signcolumn = "no"
    if thread.job and thread.buf and api.nvim_buf_is_valid(thread.buf) then
      api.nvim_win_set_buf(thread.win, thread.buf)
    else
      thread.buf = api.nvim_create_buf(false, true)
      api.nvim_win_set_buf(thread.win, thread.buf)
      local t = S.d.thread
      thread.job = vim.fn.jobstart({ t.bin, "thread", t.address, t.name }, {
        term = true,
        cwd = t.cwd,
        on_exit = function()
          vim.schedule(function()
            thread.job = nil
            thread_hide()
            if thread.buf and api.nvim_buf_is_valid(thread.buf) then
              api.nvim_buf_delete(thread.buf, { force = true })
            end
            thread.buf = nil
          end)
        end,
      })
      vim.keymap.set("t", "<C-h>", [[<C-\><C-n><C-w>h]], { buffer = thread.buf, desc = "back to the code" })
      api.nvim_create_autocmd({ "BufEnter", "WinEnter" }, { buffer = thread.buf, callback = function()
        vim.cmd("startinsert")
      end })
    end
  end
  if focus then
    api.nvim_set_current_win(thread.win)
    vim.cmd("startinsert")
  elseif api.nvim_win_is_valid(back) then
    api.nvim_set_current_win(back)
  end
end

local function thread_toggle()
  if thread.win and api.nvim_win_is_valid(thread.win) then
    thread_hide()
  else
    thread_show(true)
  end
end

-- C: a conversation with Claude about the change, in a split beside the code.
-- Claude reads the review checkout, at the MR's head, and is told where you are
-- in the tour each time you ask. It is kept per MR head, so it is there again
-- when you come back to the same change.
local chat = { buf = nil, win = nil, sid = nil, busy = false, key = nil, context = {} }
panes[#panes + 1] = chat

-- How many lines the chat holds, without the room under them.
local function chat_len()
  return api.nvim_buf_line_count(chat.buf) - PAD
end

local function chat_foot()
  if not S.d then
    return
  end
  if chat.busy then
    pane_foot(chat, "●  Claude is answering…", "OtAmber")
    return
  end
  local n = #chat.context
  local with = n > 0 and ("  ·  %d selection%s attached"):format(n, n == 1 and "" or "s") or ""
  pane_foot(chat, ("›  ask Claude about %s%s    i or enter"):format(S.d.name, with), "OtDim")
end

local function chat_files()
  local dir = vim.fn.expand("~/.cache/ot/chat")
  vim.fn.mkdir(dir, "p")
  local base = ("%s/%d-%s"):format(dir, S.d.iid, (S.d.head or ""):sub(1, 12))
  return base .. ".md", base .. ".sid"
end

local function chat_append(text)
  if not (chat.buf and api.nvim_buf_is_valid(chat.buf)) then
    return
  end
  vim.bo[chat.buf].modifiable = true
  local n = chat_len()
  local last = api.nvim_buf_get_lines(chat.buf, n - 1, n, false)[1] or ""
  local parts = vim.split(text, "\n", { plain = true })
  parts[1] = last .. parts[1]
  api.nvim_buf_set_lines(chat.buf, n - 1, n, false, parts)
  vim.bo[chat.buf].modifiable = false
  vim.fn.writefile(api.nvim_buf_get_lines(chat.buf, 0, chat_len(), false), (chat_files()))
  pane_end(chat.win, chat.buf)
end

-- Where you are, for Claude: the tour step you are reading, or the file shown.
local function chat_where()
  local i = (S.tourbuf and api.nvim_win_get_buf(S.code) == S.tourbuf and step_here()) or S.step
  local s = i and S.tour_stops[i]
  if s then
    return ("I'm reading step %d of the tour, %q, at %s:%d."):format(i, s.title or "", s.path, s.line)
  end
  if S.shown and S.shown.i and S.d.files[S.shown.i] then
    return ("I'm looking at %s."):format(S.d.files[S.shown.i].path)
  end
  return ""
end

local function chat_prompt(q)
  local where = chat_where()
  if #chat.context > 0 then
    local parts = { "Here is what I selected to ask about:" }
    for _, c in ipairs(chat.context) do
      parts[#parts + 1] = ("From %s:\n```\n%s\n```"):format(c.from, c.text)
    end
    chat.context = {}
    q = table.concat(parts, "\n\n") .. "\n\n" .. q
  end
  if chat.sid then
    return (where ~= "" and (where .. "\n\n") or "") .. q
  end
  local d = S.d
  local t = S.tour or {}
  local steps = {}
  for i, st in ipairs(t.steps or {}) do
    steps[#steps + 1] = ("%d. %s (%s:%d)"):format(i, st.title or "", st.path, st.line)
  end
  return table.concat({
    ("I'm reviewing GitLab MR %s, %q, %s into %s, by %s: %s."):format(d.name, d.title, d.source or "?", d.target or "?", d.author or "?", d.url),
    ("This directory is a checkout at the MR's head (%s). The change is `git diff %s %s`."):format(d.head or "?", d.base or "?", d.head or "?"),
    "Its description:\n\n" .. (S.description ~= "" and S.description or "(none)"),
    t.summary and ("A tour of the change says:\n\n" .. t.summary .. "\n\n" .. table.concat(steps, "\n")) or "",
    "Answer my questions about this change. Read the code to back what you say, and keep answers short. Don't edit anything.",
    where,
    q,
  }, "\n\n")
end

local function chat_ask(q)
  if chat.busy then
    S.say = { "Claude is still answering", "OtAmber" }
    render()
    return
  end
  chat_append(("%s## you\n\n%s\n\n## claude\n\n"):format(chat_len() > 1 and "\n" or "", q))
  chat.busy = true
  chat_foot()
  local cmd = { "claude", "-p", "--model", "opus", "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--strict-mcp-config", "--tools", "Read,Grep,Glob,Bash", "--allowedTools", "Read Grep Glob Bash(git diff:*) Bash(git log:*) Bash(git show:*) Bash(git blame:*)" }
  if chat.sid then
    vim.list_extend(cmd, { "--resume", chat.sid })
  end
  local pending = ""
  local function on_line(line)
    local ok, v = pcall(vim.json.decode, line)
    if not ok or type(v) ~= "table" then
      return
    end
    if v.session_id and v.session_id ~= chat.sid then
      chat.sid = v.session_id
      vim.fn.writefile({ chat.sid }, select(2, chat_files()))
    end
    local e = v.type == "stream_event" and v.event or nil
    if e and e.type == "content_block_delta" and e.delta and e.delta.type == "text_delta" then
      chat_append(e.delta.text)
    elseif e and e.type == "content_block_start" and e.content_block and e.content_block.type == "tool_use" then
      chat_append(("\n_(%s)_\n\n"):format(e.content_block.name))
    elseif v.type == "result" and v.is_error then
      chat_append("\n\n**" .. tostring(v.result or "Claude stopped") .. "**")
    end
  end
  vim.system(cmd, {
    cwd = S.root or vim.fn.getcwd(),
    stdin = chat_prompt(q),
    stdout = function(_, data)
      if not data then
        return
      end
      pending = pending .. data
      local lines = {}
      while true do
        local nl = pending:find("\n", 1, true)
        if not nl then
          break
        end
        lines[#lines + 1] = pending:sub(1, nl - 1)
        pending = pending:sub(nl + 1)
      end
      vim.schedule(function()
        for _, l in ipairs(lines) do
          on_line(l)
        end
      end)
    end,
  }, function(r)
    vim.schedule(function()
      chat.busy = false
      chat_foot()
      if r.code ~= 0 and r.stderr and r.stderr ~= "" then
        chat_append("\n\n**claude: " .. vim.trim(r.stderr) .. "**")
      end
      chat_append("\n")
    end)
  end)
end

local function chat_input()
  vim.ui.input({ prompt = "Ask Claude about " .. S.d.name .. ": " }, function(q)
    if q and vim.trim(q) ~= "" then
      chat_ask(vim.trim(q))
    end
  end)
end

local function chat_show()
  local md, sidf = chat_files()
  if chat.key ~= md then
    chat.key, chat.sid = md, nil
    if chat.buf and api.nvim_buf_is_valid(chat.buf) then
      api.nvim_buf_delete(chat.buf, { force = true })
    end
    chat.buf = nil
    if vim.fn.filereadable(sidf) == 1 then
      chat.sid = vim.fn.readfile(sidf)[1]
    end
  end
  if not (chat.buf and api.nvim_buf_is_valid(chat.buf)) then
    chat.buf = api.nvim_create_buf(false, true)
    api.nvim_buf_set_name(chat.buf, ("ot://%s/chat"):format(S.d.name))
    vim.bo[chat.buf].filetype = "markdown"
    local kept = vim.fn.filereadable(md) == 1 and vim.fn.readfile(md) or { "" }
    api.nvim_buf_set_lines(chat.buf, 0, -1, false, padded(kept))
    vim.bo[chat.buf].modifiable = false
    for _, k in ipairs({ "C", "i", "<CR>" }) do
      vim.keymap.set("n", k, chat_input, { buffer = chat.buf, nowait = true, desc = "ask Claude" })
    end
    vim.keymap.set("n", "q", function()
      api.nvim_win_close(0, true)
    end, { buffer = chat.buf, nowait = true, desc = "close the chat" })
  end
  if not (chat.win and api.nvim_win_is_valid(chat.win)) then
    -- Under the T3 thread when it shows, so the two share the right of the screen.
    if thread.win and api.nvim_win_is_valid(thread.win) then
      api.nvim_set_current_win(thread.win)
      vim.cmd("belowright split")
    else
      vim.cmd("botright vsplit")
      api.nvim_win_set_width(0, math.max(50, math.floor(vim.o.columns * 0.35)))
    end
    chat.win = api.nvim_get_current_win()
    api.nvim_win_set_buf(chat.win, chat.buf)
    vim.wo[chat.win].wrap, vim.wo[chat.win].linebreak = true, true
    vim.wo[chat.win].number, vim.wo[chat.win].relativenumber = false, false
    vim.wo[chat.win].winbar = "%#OtDim# chat about " .. S.d.name .. "%=C/i/enter ask  q close "
    api.nvim_create_autocmd("WinClosed", { pattern = tostring(chat.win), once = true, callback = function()
      vim.schedule(function()
        pane_foot_close(chat)
      end)
    end })
  end
  chat_foot()
  api.nvim_set_current_win(chat.win)
  pane_end(chat.win, chat.buf)
end

local function chat_open()
  chat_show()
  chat_input()
end

-- <leader>ma on a selection: its lines go to the chat, which opens beside the
-- code if it isn't open, and asks your question about them.
local function chat_add()
  local buf, mode = api.nvim_get_current_buf(), vim.fn.mode()
  local from_pos, to_pos = vim.fn.getpos("v"), vim.fn.getpos(".")
  local text = table.concat(vim.fn.getregion(from_pos, to_pos, { type = mode }), "\n")
  api.nvim_feedkeys(api.nvim_replace_termcodes("<Esc>", true, false, true), "nx", false)
  local a, b = math.min(from_pos[2], to_pos[2]), math.max(from_pos[2], to_pos[2])
  local from = "the tour"
  for f, fb in pairs(S.bufs) do
    if fb == buf then
      from = ("%s:%d-%d"):format(S.d.files[f].path, a, b)
    end
  end
  if from == "the tour" and buf ~= S.tourbuf then
    local name = api.nvim_buf_get_name(buf)
    local root = S.root and (S.root .. "/")
    from = ((root and name:sub(1, #root) == root) and name:sub(#root + 1) or name) .. (":%d-%d"):format(a, b)
  end
  if vim.trim(text) == "" then
    return
  end
  local back = api.nvim_get_current_win()
  chat_show()
  chat.context[#chat.context + 1] = { from = from, text = text }
  local n = select(2, text:gsub("\n", "")) + 1
  chat_append(("%s_(added %s, %d line%s, for your next question)_\n"):format(chat_len() > 1 and "\n" or "", from, n, n == 1 and "" or "s"))
  chat_foot()
  if api.nvim_win_is_valid(back) then
    api.nvim_set_current_win(back)
  end
  S.say = { ("added %s to the chat"):format(from), "OtGreen" }
  render()
  -- Straight to the question; esc keeps it for one asked later, with more added.
  chat_input()
end

tour_acts = {
  NextStep = list_acts.NextStep,
  PrevStep = list_acts.PrevStep,
  Into = into_step,
  Chat = chat_open,
  ThreadPane = thread_toggle,
  Leave = leave,
}
list_acts.Chat = chat_open
list_acts.ThreadPane = thread_toggle
-- An MR a T3 thread works on opens with its conversation beside the code.
open_thread_pane = function()
  if S.d and S.d.thread then
    thread_show()
  end
end
code_acts.ChatAdd = chat_add

-- The keys that work in a window, one word each, from ot's one table, as
-- pieces for pack().
-- lualine and the like own the statusline, so these go where they don't:
-- the list's header, and the code's winbar beside its file's path.
-- A place's keys and their words, as pieces for a line.
key_words = function(place, acts)
  local out = {}
  for _, k in ipairs(S.d.keys) do
    if k.word ~= "" and vim.tbl_contains(k.on, place) and acts[k.act] then
      out[#out + 1] = { { as_typed(k.key), "OtText" }, { " " .. k.word, "OtDim" } }
    end
  end
  return out
end

keys_line = function(place, acts)
  local out = key_words(place, acts)
  out[#out + 1] = { { "?", "OtText" }, { " keys", "OtDim" } }
  return out
end

winbar = function(f)
  local esc = function(s)
    return (s:gsub("%%", "%%%%"))
  end
  local file = S.d.files[f]
  -- The path under the directory the list names, so it fits; the keys
  -- only when they fit beside it, since nvim cuts a winbar that doesn't.
  local path = file.path:sub(#shared_dir() + 1) .. (file.status == "A" and "  a new file" or (file.status == "D" and "  removed" or ""))
  -- A copy has no LSP; say so until the real file takes its place.
  if S.bufs[f] and vim.bo[S.bufs[f]].buftype == "nofile" and not file.gone then
    path = path .. "  · a copy until the checkout lands"
  end
  local keys = table.concat(vim.tbl_map(function(piece)
    return piece[1][1] .. piece[2][1]
  end, keys_line("Code", code_acts)), "  ")
  local room = api.nvim_win_get_width(S.code) - W(path) - W(keys) - 4
  local bar = "%#OtDim# " .. esc(path) .. (room >= 0 and ("%=" .. esc(keys) .. " ") or "")
  -- Set only when it changes, since setting it redraws it.
  if vim.wo[S.code].winbar ~= bar then
    vim.wo[S.code].winbar = bar
  end
end

local function landed(d)
  if d.note then
    S.note = d.note
    render()
    return false
  end
  if d.error then
    S.note = d.error
    render()
    return true
  end
  S.d, S.note = d, nil
  for _, m in ipairs(d.viewed) do
    S.viewed[m] = true
  end
  api.nvim_buf_set_name(S.listbuf, ("ot://!%d"):format(d.iid))
  map_keys(S.listbuf, "List", list_acts)
  render()
  vim.schedule(open_thread_pane)
  if S.waiting then
    S.waiting(S.waiting_with)
  end
  return true
end

-- What has been said on the MR, from the file ot wrote at the start or
-- from its answer to something you sent: drawn wherever it shows.
function M.talk(t)
  if not S.d then
    -- The change hasn't landed yet: this waits for it.
    S.waiting, S.waiting_with = M.talk, t
    return true
  end
  S.waiting = nil
  S.threads, S.drafts, S.open_threads = t.threads or {}, t.drafts or {}, t.open_threads or {}
  S.description = t.description or S.description
  S.threads_error = t.error
  draw()
  if S.overview then
    overview()
  end
  render()
  return true
end

-- The review checkout at the MR's head: the copies give way to the real
-- files, the code staying where you were looking.
local function tree(t)
  if t.note then
    S.tree_note = t.note
    if S.d then
      render()
    end
    return false
  end
  S.tree_note = t.error
  if t.root then
    S.root = t.root
    -- nvim works from the checkout: a search, a jump, your LSP all find
    -- the MR's files, not the ones on whatever branch you are on.
    api.nvim_set_current_dir(t.root)
    local view = S.shown and api.nvim_win_call(S.code, vim.fn.winsaveview)
    local old = S.bufs
    S.bufs = {}
    if S.shown then
      local again, hunk = S.shown, S.hunk
      S.shown = nil
      show(again)
      S.hunk = hunk
      api.nvim_win_call(S.code, function()
        vim.fn.winrestview(view)
      end)
    end
    for _, b in pairs(old) do
      if api.nvim_buf_is_valid(b) and vim.bo[b].buftype == "nofile" then
        api.nvim_buf_delete(b, { force = true })
      end
    end
  end
  if S.d then
    render()
  end
  return true
end

-- The foot of the list: cliamp's hills, FOOT rows under the rows, as the
-- board draws them, while music plays or is paused and the column has room
-- for both. ot draws them at the size this asks for; a frame lands in
-- S.hills and is painted when the foot is there.
local FOOT = 9

local function foot_paint()
  if not S.foot or not S.hills then
    return
  end
  local texts, hls = {}, {}
  for i, r in ipairs(S.hills.rows) do
    local hl = "OtHill" .. r.fg:sub(2)
    if not S.hill_hl[hl] then
      api.nvim_set_hl(0, hl, { fg = r.fg })
      S.hill_hl[hl] = true
    end
    texts[i], hls[i] = r.text, { { 0, #r.text, hl } }
  end
  set_lines(api.nvim_win_get_buf(S.foot), texts, hls)
end

local function foot_size()
  local cols = S.foot and api.nvim_win_get_width(S.foot) or 0
  if cols ~= S.foot_cols then
    S.foot_cols = cols
    call({ op = "hills", cols = cols, depth = FOOT }, function() end)
  end
end

local function foot()
  local m = S.music
  local on = m and (m.state == "playing" or m.state == "paused")
  local room = api.nvim_win_get_height(S.list) + (S.foot and FOOT + 1 or 0) >= 24
  if on and room and not S.foot then
    local buf = api.nvim_create_buf(false, true)
    vim.bo[buf].buftype, vim.bo[buf].bufhidden, vim.bo[buf].swapfile = "nofile", "wipe", false
    S.foot = api.nvim_open_win(buf, false, { split = "below", win = S.list, height = FOOT, focusable = false })
    local wo = vim.wo[S.foot]
    wo.number, wo.relativenumber, wo.signcolumn, wo.foldcolumn, wo.statuscolumn = false, false, "no", "0", ""
    wo.cursorline, wo.wrap, wo.list, wo.winfixheight = false, false, false, true
    api.nvim_create_autocmd("WinClosed", {
      pattern = tostring(S.foot),
      once = true,
      callback = function()
        S.foot = nil
        foot_size()
      end,
    })
    foot_paint()
  elseif not (on and room) and S.foot then
    api.nvim_win_close(S.foot, true)
  end
  foot_size()
end

local function setup()
  colors()
  S.code = api.nvim_get_current_win()
  -- The name nvim was started on, to keep a dashboard away: gone once a
  -- file takes its place.
  vim.bo[api.nvim_win_get_buf(S.code)].bufhidden = "wipe"
  vim.cmd("topleft vertical new")
  S.list, S.listbuf = api.nvim_get_current_win(), api.nvim_get_current_buf()
  vim.bo[S.listbuf].buftype = "nofile"
  -- Not a buffer to switch to: off :ls and any buffer line.
  vim.bo[S.listbuf].buflisted = false
  -- Kept while ? shows the keys in its place.
  vim.bo[S.listbuf].bufhidden = "hide"
  vim.bo[S.listbuf].swapfile = false
  vim.bo[S.listbuf].filetype = "ot"
  api.nvim_win_set_width(S.list, math.max(30, math.min(44, math.floor(vim.o.columns * 0.35))))
  local wo = vim.wo[S.list]
  wo.number, wo.relativenumber, wo.signcolumn, wo.foldcolumn = false, false, "no", "0"
  -- The list lays its own lines out and draws its own band.
  wo.wrap, wo.cursorline, wo.winfixwidth = false, false, true
  wo.list, wo.spell, wo.statuscolumn, wo.winhighlight = false, false, "", "CursorLine:OtBand"
  -- mini.indentscope takes the rails' indent for a scope and redraws its
  -- line a moment after every move, which flickers.
  vim.b[S.listbuf].miniindentscope_disable = true
  -- Absolute numbers: relative ones count past removed lines as if they
  -- were not there. The cursor's line lit, as the list lights its row.
  vim.wo[S.code].number = true
  vim.wo[S.code].relativenumber = false
  vim.wo[S.code].cursorline = true
  -- One statusline, the screen's: the foot sits under the list with only
  -- a rule between, not a statusline of the list's.
  vim.o.laststatus = 3
  S.note = "reading the change…"
  render()
  local group = api.nvim_create_augroup("ot", {})
  -- No cursor in the list, whose band says where you are; yours again in
  -- the code. Command-line modes keep theirs, so : still shows one.
  local yours = vim.o.guicursor
  local hidden = yours .. ",n-v-sm:block-OtHidden"
  api.nvim_create_autocmd("WinEnter", {
    group = group,
    callback = function()
      vim.o.guicursor = api.nvim_get_current_win() == S.list and hidden or yours
    end,
  })
  vim.o.guicursor = hidden
  -- nested: a click on a row, or gg, shows its file from in here, and a
  -- file read inside an autocommand gets no filetype (so no colors, no
  -- LSP) unless its own autocommands are let run.
  api.nvim_create_autocmd("CursorMoved", { group = group, buffer = S.listbuf, nested = true, callback = M.follow })
  api.nvim_create_autocmd("BufEnter", { group = group, callback = M.keep_to_window })
  api.nvim_create_autocmd("BufEnter", { group = group, callback = M.adopt_shown })
  api.nvim_create_autocmd("BufWritePost", { group = group, callback = M.saved })
  -- The two windows are the review: closing either ends it.
  api.nvim_create_autocmd("WinClosed", {
    group = group,
    callback = function(ev)
      local w = tonumber(ev.match)
      if w == S.list or w == S.code then
        leave()
      end
    end,
  })
  api.nvim_create_autocmd("CursorMoved", { group = group, callback = bar })
  api.nvim_create_autocmd("ColorScheme", { group = group, callback = colors })
  -- WinResized too: the code narrows as the list widens to its names.
  api.nvim_create_autocmd({ "VimResized", "WinResized" }, {
    group = group,
    callback = function()
      foot()
      if S.d then
        draw()
        -- The tour's page is wrapped to the window, and its machine sized
        -- to it.
        if S.tour and S.tourbuf then
          tour_page()
        end
        bar_again()
        render()
      end
    end,
  })
  watch(vim.env.OT_REVIEW, landed)
  watch(vim.env.OT_TALK, M.talk)
  watch(vim.env.OT_TREE, tree)
  watch(vim.env.OT_TOUR, function(t)
    local first = S.tour == nil
    S.tour = t
    if S.d then
      -- You opened it for the tour, so the review starts on its page,
      -- unless you have already gone to read something.
      if first and (not S.shown or S.shown.kind == "overview") then
        S.at = "tour"
      elseif S.tourbuf then
        tour_page()
      end
      render()
    end
    return t.error ~= nil or not t.reading
  end)
  -- For as long as the review is open: the line changes when the track or
  -- its state does, the player with every clock tick.
  watch(vim.env.OT_HILLS, function(h)
    S.hills = h
    foot_paint()
    return false
  end)
  watch(vim.env.OT_MUSIC, function(m)
    local was = S.music
    S.music = m
    foot()
    player_draw()
    local function said(x)
      return x and (x.title .. "\n" .. (x.artist or "") .. "\n" .. x.state .. "\n" .. tostring(x.buffering)) or ""
    end
    if S.d and said(m) ~= said(was) then
      render()
    end
    return false
  end)
end

-- Starts the review once nvim is up, after your config and anything it
-- opens on startup, and takes the screen over from there.
function M.open()
  if vim.v.vim_did_enter == 1 then
    setup()
  else
    api.nvim_create_autocmd("VimEnter", {
      once = true,
      callback = function()
        vim.schedule(setup)
      end,
    })
  end
end

return M
