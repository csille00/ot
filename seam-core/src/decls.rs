//! One row a declaration, from ast-grep's matches on the language rules in
//! share/languages: what share/decls.jq makes of `ast-grep scan --json`.
use ast_grep_config::{from_yaml_string, CombinedScan, GlobalRules, RuleConfig};
use ast_grep_core::tree_sitter::LanguageExt;
use ast_grep_core::Language;
use ast_grep_language::SupportLang;
use regex::Regex;
use std::collections::HashMap;
use std::path::Path;

/// A language file: its #seam: lines and its rules.
pub struct Lang {
    pub name: String,
    pub extensions: Vec<String>,
    pub grammars: Vec<String>,
    noise: Option<Regex>,
    keys: Option<String>,
    /// Its files are ES modules: a name one mentions is another file's only
    /// through an import (#seam: imports es).
    pub imports: bool,
    yml: String,
}

impl Lang {
    pub fn new(name: &str, yml: &str) -> Result<Lang, String> {
        let mut f: HashMap<&str, &str> = HashMap::new();
        for l in yml.lines() {
            let Some(rest) = l.strip_prefix("#seam: ") else { continue };
            let n = rest.find(|c: char| !c.is_ascii_lowercase()).unwrap_or(rest.len());
            if n > 0 {
                f.insert(&rest[..n], rest[n..].strip_prefix(' ').unwrap_or(&rest[n..]));
            }
        }
        let words = |k: &str| f.get(k).map(|v| v.split(' ').filter(|w| !w.is_empty()).map(String::from).collect()).unwrap_or_default();
        let noise = match f.get("noise") {
            Some(n) => Some(Regex::new(n).map_err(|e| format!("{name}: noise: {e}"))?),
            None => None,
        };
        Ok(Lang { name: name.into(), extensions: words("extensions"), grammars: words("grammars"), noise, keys: f.get("keys").map(|k| k.to_string()), imports: f.get("imports") == Some(&"es"), yml: yml.into() })
    }
}

/// A declaration as the table reads it. Strings are escaped as a tab-separated
/// row has them (see tsv), since they are written back the same.
pub struct Decl {
    pub path: String,
    pub rule: String,
    pub start: u32,
    pub end: u32,
    pub name: String,
    pub head: String,
    pub text: String,
}

pub struct Rules {
    langs: Vec<Lang>,
    // A grammar's rules, in the order the language file gives them.
    by_grammar: Vec<(SupportLang, Vec<RuleConfig<SupportLang>>)>,
    template: Regex,
    interp: Regex,
    noise: Regex,
}

impl Rules {
    pub fn new(langs: Vec<Lang>) -> Result<Rules, String> {
        let mut by_grammar: Vec<(SupportLang, Vec<RuleConfig<SupportLang>>)> = vec![];
        for l in &langs {
            for g in &l.grammars {
                let yml: String = l.yml.lines().map(|x| if x == "language: LANG" { format!("language: {g}\n") } else { format!("{x}\n") }).collect();
                let configs = from_yaml_string::<SupportLang>(&yml, &GlobalRules::default()).map_err(|e| format!("{}: {e:#}", l.name))?;
                for c in configs {
                    match by_grammar.iter_mut().find(|(lang, _)| *lang == c.language) {
                        Some((_, v)) => v.push(c),
                        None => by_grammar.push((c.language, vec![c])),
                    }
                }
            }
        }
        Ok(Rules {
            langs,
            by_grammar,
            template: Regex::new(r"`((?:[^`\\]|\\.)*)`").unwrap(),
            interp: Regex::new(r"\$\{((?:[^{}]|\{[^{}]*\})*)\}").unwrap(),
            noise: Regex::new(r#"/\*[\s\S]*?\*/|//[^\n]*|"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*'"#).unwrap(),
        })
    }

    /// The language file that reads a path, by its extension; the first wins.
    pub fn lang_of(&self, path: &str) -> Option<&Lang> {
        self.langs.iter().find(|l| l.extensions.iter().any(|e| path.ends_with(e.as_str())))
    }

    /// The code alone: a name in a string or a comment is not a use of it.
    fn code(&self, text: &str, lang: Option<&Lang>) -> String {
        let mut s = match lang.and_then(|l| l.noise.as_ref()) {
            Some(noise) => noise.replace_all(text, " ").into_owned(),
            None => {
                let t = self.template.replace_all(text, |c: &regex::Captures| {
                    let inner: Vec<&str> = self.interp.captures_iter(&c[1]).map(|m| m.get(1).unwrap().as_str()).collect();
                    format!(" {} ", inner.join(" "))
                });
                self.noise.replace_all(&t, " ").into_owned()
            }
        };
        s = s.replace("::", " ");
        let keys = lang.and_then(|l| l.keys.as_deref()).unwrap_or(":?");
        for c in [':', '?'] {
            if !keys.contains(c) {
                s = s.replace(c, " ");
            }
        }
        s
    }

    /// Every declaration in one file's text.
    pub fn scan(&self, path: &str, src: &str, out: &mut Vec<Decl>) {
        let Some(lang) = SupportLang::from_path(Path::new(path)) else { return };
        let Some((_, rules)) = self.by_grammar.iter().find(|(l, _)| *l == lang) else { return };
        let ext = path.rfind('.').filter(|&i| !path[i..].contains('/')).map(|i| &path[i..]).unwrap_or("");
        let l = self.langs.iter().find(|l| l.extensions.iter().any(|e| e == ext));
        let root = lang.ast_grep(src);
        let scan = CombinedScan::new(rules.iter().collect());
        let mut found = scan.scan(&root, false).matches;
        // ast-grep hands the rules back in no order; the file's own is one.
        found.sort_by_key(|(r, _)| rules.iter().position(|x| std::ptr::eq(x, *r)));
        for (rule, matches) in found {
            let r = rule.id.as_str();
            for m in matches {
                let text = m.text();
                let lines: Vec<&str> = text.split('\n').collect();
                let (sp, ep) = (m.start_pos(), m.end_pos());
                let (s, sc) = (sp.line(), sp.column(&m));
                // A node that takes its line's newline with it (a Rust //
                // comment) ends on the line before the one its range ends on.
                let e = ep.line() + if ep.column(&m) == 0 && ep.line() > s { 0 } else { 1 };
                let env = m.get_env();
                let head = if r == "export" || r == "comment" {
                    String::new()
                } else if let Some(b) = env.get_match("B") {
                    let (bs, be) = (b.start_pos(), b.end_pos());
                    let k = bs.line() - s;
                    let c = if k == 0 { bs.column(b) - sc } else { bs.column(b) };
                    let at = lines.get(k).copied().unwrap_or("");
                    let cut = at.char_indices().nth(c).map(|(i, _)| i).unwrap_or(at.len());
                    let mut pre: Vec<&str> = lines[..k.min(lines.len())].to_vec();
                    pre.push(&at[..cut]);
                    let pre = pre.join(" ");
                    if r == "const" || r == "type" {
                        format!("{pre} {}{}", &at[cut..], if be.line() > bs.line() { "…" } else { "" })
                    } else {
                        pre
                    }
                } else {
                    format!("{}{}", lines[0], if lines.len() > 1 { "…" } else { "" })
                };
                let head = clean(&head);
                // A rule with no $N (a Rust impl) is named by its interface.
                let name = match env.get_match("N") {
                    Some(n) => n.text().squash(),
                    None => head.squash(),
                };
                let code = if matches!(r, "export" | "comment" | "method" | "field") { String::new() } else { self.code(&text, l) };
                out.push(Decl { path: path.into(), rule: r.into(), start: s as u32 + 1, end: e as u32, name: tsv(&name), head: tsv(&head), text: tsv(&code) });
            }
        }
    }
}

trait Squash {
    fn squash(&self) -> String;
}
impl<T: AsRef<str>> Squash for T {
    /// Each run of whitespace as one space, the ends kept.
    fn squash(&self) -> String {
        let mut out = String::new();
        let mut was = false;
        for c in self.as_ref().chars() {
            if c.is_whitespace() {
                if !was {
                    out.push(' ');
                }
                was = true;
            } else {
                out.push(c);
                was = false;
            }
        }
        out
    }
}

/// Whitespace first, then the spaces a bracket or a comma does not want, then
/// the arrow a const's head ends on.
fn clean(head: &str) -> String {
    let sq: Vec<char> = head.split_whitespace().collect::<Vec<_>>().join(" ").chars().collect();
    let mut out = String::new();
    for (i, &c) in sq.iter().enumerate() {
        if c == ' ' && (sq.get(i.wrapping_sub(1)) == Some(&'(') || matches!(sq.get(i + 1), Some(',') | Some(')'))) {
            continue;
        }
        out.push(c);
    }
    let t = out.trim_end();
    match t.strip_suffix("=>") {
        Some(t) => t.trim_end().to_string(),
        None => out,
    }
}

/// A field of a tab-separated row, as jq's @tsv writes one.
pub fn tsv(s: &str) -> String {
    if !s.contains(['\\', '\t', '\n', '\r']) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}
