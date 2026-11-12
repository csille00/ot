// A tab nothing opened (ot's w, a Slack link, a URL typed into a new tab)
// switches to the tab already showing that page, if there is one.
// Links opened from a page have an opener and are left alone.

// New tabs still on about:blank or the new tab page, waiting for their URL.
const waiting = new Set();

browser.tabs.onCreated.addListener((tab) => {
  if (tab.openerTabId !== undefined) return;
  if (tab.url.startsWith("http")) reuse(tab);
  else waiting.add(tab.id);
});

browser.tabs.onUpdated.addListener(
  (id, change, tab) => {
    if (waiting.has(id) && change.url.startsWith("http")) {
      waiting.delete(id);
      reuse(tab);
    }
  },
  { properties: ["url"] },
);

browser.tabs.onRemoved.addListener((id) => waiting.delete(id));

async function reuse(tab) {
  if (tab.url.includes("#")) return; // a link to a comment should land on it
  const want = page(tab.url);
  const [old] = (await browser.tabs.query({}))
    .filter((t) => t.id !== tab.id && page(t.url) === want)
    .sort((a, b) => b.lastAccessed - a.lastAccessed);
  if (!old) return;
  await browser.tabs.update(old.id, { active: true });
  await browser.windows.update(old.windowId, { focused: true });
  await browser.tabs.remove(tab.id);
}

// An MR is one page whichever of its tabs (/diffs, /commits) is showing.
function page(url) {
  return url.match(/^[^#?]*\/-\/merge_requests\/\d+/)?.[0] ?? url.split("#")[0];
}
