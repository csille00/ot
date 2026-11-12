//! What a file written in ES modules imports, so that a name it mentions is
//! looked for where an import says it comes from: a changed `cancelled` in
//! one file is not what another file's `let cancelled` means.
use regex::Regex;
use std::sync::OnceLock;

/// A file's imports.
#[derive(Default)]
pub struct Imports {
    /// Each name imported under its own name, and the module it is from. One
    /// imported under another name (`a as b`) brings nothing in as either.
    named: Vec<(String, String)>,
    /// The modules imported whole (`* as m`), which a destructure or a typeof
    /// can take any name from.
    whole: Vec<String>,
}

impl Imports {
    /// The imports in a file's text: each `import … from "…"` that opens a
    /// line, which in formatted code is each import statement.
    pub fn read(text: &str) -> Imports {
        static STATEMENT: OnceLock<Regex> = OnceLock::new();
        static COMMENT: OnceLock<Regex> = OnceLock::new();
        let statement = STATEMENT.get_or_init(|| Regex::new(r#"(?m)^import\s+(?:type\s+)?([^;"'`]*?)\s*\bfrom\s*["']([^"']+)["']"#).unwrap());
        let comment = COMMENT.get_or_init(|| Regex::new(r"/\*[\s\S]*?\*/|//[^\n]*").unwrap());
        let mut out = Imports::default();
        for c in statement.captures_iter(text) {
            let spec = &c[2];
            let clause = comment.replace_all(&c[1], " ").split_whitespace().collect::<Vec<_>>().join(" ");
            // A default import, then what the braces name, or a module whole.
            for part in clause.split(['{', '}', ',']).map(str::trim) {
                let part = part.strip_prefix("type ").unwrap_or(part);
                if part.starts_with('*') {
                    out.whole.push(spec.to_string());
                } else if !part.is_empty() && !part.contains(" as ") {
                    out.named.push((part.to_string(), spec.to_string()));
                }
            }
        }
        out
    }

    /// Whether a mention of `name` in the file at `from` can be the definition
    /// of that name in the file at `to`: the file imports the name from a
    /// module that can be that file, or imports a module whole that can be.
    /// A module seam cannot place is taken at its word when it is imported by
    /// name, and when imported whole (React, most often) says nothing.
    pub fn brings(&self, name: &str, from: &str, to: &str) -> bool {
        self.named.iter().any(|(n, spec)| n == name && reaches(spec, from, to).unwrap_or(true))
            || self.whole.iter().any(|spec| reaches(spec, from, to).unwrap_or(false))
    }
}

/// Whether a module specifier written in the file at `from` can name the file
/// at `to`: a relative one names that file or a directory above it, whose
/// index can re-export it, and a package's subpath ends that file's path or a
/// directory's above it, since where a package lives is the repo's to say.
/// None for a package's root, which could be anywhere.
fn reaches(spec: &str, from: &str, to: &str) -> Option<bool> {
    // An ES module may name a .ts file by the .js it compiles to, and an
    // index by its directory, which is what it re-exports from.
    let code = |p: &str| -> String {
        let p = match p.rsplit_once('.') {
            Some((stem, ext)) if matches!(ext, "ts" | "tsx" | "js" | "jsx" | "mts" | "cts" | "mjs" | "cjs") && !stem.is_empty() && !stem.ends_with('/') => stem,
            _ => p,
        };
        p.strip_suffix("/index").unwrap_or(p).to_string()
    };
    let file = code(to);
    if spec.starts_with('.') {
        let mut at: Vec<&str> = from.split('/').collect();
        at.pop();
        for seg in spec.split('/') {
            match seg {
                "" | "." => {}
                ".." => {
                    at.pop();
                }
                s => at.push(s),
            }
        }
        let place = code(&at.join("/"));
        return Some(place == file || to.starts_with(&format!("{place}/")));
    }
    let n = if spec.starts_with('@') { 2 } else { 1 };
    let sub = code(spec.splitn(n + 1, '/').nth(n).filter(|s| !s.is_empty())?);
    Some(format!("/{file}").ends_with(&format!("/{sub}")) || format!("/{to}").contains(&format!("/{sub}/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FROM: &str = "app/src/panels/note-panel.tsx";

    #[test]
    fn a_name_is_another_files_only_when_imported_from_there() {
        let i = Imports::read(
            "import { memo } from \"react\";\n\
             import type { NoteSource } from \"@acme/api-sdk/schema/fetch-note-info\";\n\
             import {\n  NoteDetails, // the card\n  type NoteUsage,\n  badge as statusBadge,\n} from \"./note-details\";\n\
             import * as Activities from \"../activities\";\n\
             import * as React from \"react\";\n",
        );
        // Imported by name, from that file.
        assert!(i.brings("NoteDetails", FROM, "app/src/panels/note-details.tsx"));
        assert!(i.brings("NoteUsage", FROM, "app/src/panels/note-details.tsx"));
        assert!(i.brings("NoteSource", FROM, "api/sdk/src/schema/fetch-note-info.ts"));
        // The same name in a file the import does not name: a schema's handler.
        assert!(!i.brings("NoteSource", FROM, "api/server/src/handler/fetch-note-info.ts"));
        // Never imported: a local `let cancelled` is not another file's.
        assert!(!i.brings("cancelled", FROM, "jobs/src/workflows/review-and-send.ts"));
        // Imported under another name, it is neither.
        assert!(!i.brings("badge", FROM, "app/src/panels/note-details.tsx"));
        assert!(!i.brings("statusBadge", FROM, "app/src/panels/note-details.tsx"));
        // A module imported whole can bring any name, from under its own directory.
        assert!(i.brings("processTrigger", FROM, "app/src/activities/trigger.ts"));
        // A package's root could be anything: by name it is believed, whole it is not.
        assert!(i.brings("memo", FROM, "react/src/memo.ts"));
        assert!(!i.brings("useState", FROM, "web/src/hooks.ts"));
    }

    #[test]
    fn a_specifier_names_a_file_or_a_directory_above_it() {
        let to = "acme/api/sdk/src/rpc/schema/billing/index.ts";
        assert_eq!(reaches("./billing", "acme/api/sdk/src/rpc/schema/x.ts", to), Some(true));
        assert_eq!(reaches("../schema/billing/index.js", "acme/api/sdk/src/rpc/y/x.ts", to), Some(true));
        assert_eq!(reaches("./billing/index", "acme/api/sdk/src/rpc/schema/x.ts", "acme/api/sdk/src/rpc/schema/billing/plans.ts"), Some(true));
        assert_eq!(reaches("./billing-v2", "acme/api/sdk/src/rpc/schema/x.ts", to), Some(false));
        assert_eq!(reaches("@acme/api-sdk/rpc/schema/billing", "acme/a.ts", to), Some(true));
        assert_eq!(reaches("@acme/api-sdk/rpc/schema", "acme/a.ts", to), Some(true));
        assert_eq!(reaches("@acme/api-sdk/rpc/schema/bill", "acme/a.ts", to), Some(false));
        assert_eq!(reaches("@acme/api-sdk", "acme/a.ts", to), None);
        assert_eq!(reaches("zod", "acme/a.ts", to), None);
    }
}
