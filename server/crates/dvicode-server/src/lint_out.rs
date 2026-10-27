//! The output formats of `dvicode lint`: text, JSON, SARIF 2.1.0, JUnit XML and GitHub
//! workflow annotations, from one list of findings.

use dvi_analysis::lint::{rules, Level};
use lsp_types::Range;
use serde_json::{json, Value};

/// How a finding is kept from failing the run.
#[derive(Clone, Debug, PartialEq)]
pub enum Kept {
    No,
    /// A waiver or a `dvicode-lint:` suppression in the code (what it says).
    InSource(String),
    /// The baseline lists it.
    Baseline,
}

/// A finding as reported.
#[derive(Clone, Debug)]
pub struct Item {
    pub rule: &'static str,
    pub subcheck: Option<&'static str>,
    pub level: Level,
    pub message: String,
    /// Absolute, as shown in text output.
    pub path: String,
    /// Relative to the project root, `/`-separated.
    pub rel: String,
    pub range: Option<Range>,
    /// Related places: relative path, range, message.
    pub related: Vec<(String, Range, String)>,
    pub contexts: Vec<Value>,
    pub kept: Kept,
    /// A stable hash of the finding's fingerprint (rule, file, declaration, text and the
    /// occurrence among equal ones), and one of the lines around it.
    pub fingerprint: String,
    pub context_fingerprint: String,
    /// The path is outside the project root (`rel` is then absolute).
    pub outside: bool,
}

impl Item {
    pub fn counts(&self) -> bool {
        self.kept == Kept::No
    }

    fn tag(&self) -> String {
        match rules::meta(self.rule).and_then(|m| m.guide.first()) {
            Some(g) => format!("{}/{g}", self.rule),
            None => self.rule.to_string(),
        }
    }

    fn line_col(&self) -> (u32, u32) {
        self.range.map_or((1, 1), |r| (r.start.line + 1, r.start.character + 1))
    }
}

pub fn text(items: &[Item], show_kept: bool) -> String {
    let mut s = String::new();
    for x in items.iter().filter(|x| x.counts() || show_kept) {
        let (line, col) = x.line_col();
        let kept = match &x.kept {
            Kept::No => String::new(),
            Kept::InSource(w) => format!(" (suppressed: {w})"),
            Kept::Baseline => " (in the baseline)".into(),
        };
        s.push_str(&format!("{}:{line}:{col}: {}: {} [{}]{kept}\n", x.path, x.level.as_str(), x.message, x.tag()));
    }
    s
}

pub fn json_items(items: &[Item]) -> Vec<Value> {
    items
        .iter()
        .map(|x| {
            json!({
                "rule": x.rule,
                "subcheck": x.subcheck,
                "level": x.level,
                "message": x.message,
                "path": x.rel,
                "range": x.range,
                "contexts": x.contexts,
                "related": x.related.iter().map(|(p, r, m)| json!({ "path": p, "range": r, "message": m })).collect::<Vec<_>>(),
                "suppressed": match &x.kept { Kept::No => Value::Null, Kept::InSource(w) => json!(w), Kept::Baseline => json!("baseline") },
                "fingerprint": x.fingerprint,
            })
        })
        .collect()
}

fn sarif_level(l: Level) -> &'static str {
    match l {
        Level::Error => "error",
        Level::Warning => "warning",
        Level::Info | Level::Hint => "note",
        Level::Off => "none",
    }
}

/// A path as a URI reference: each segment percent-encoded (RFC 3986 unreserved characters
/// kept), `/` between them.
pub fn uri_path(p: &str) -> String {
    let mut o = String::new();
    for b in p.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

/// A SARIF artifact location: relative to `%SRCROOT%` inside the project, an absolute file URI
/// outside it.
fn artifact(rel: &str, outside: bool) -> Value {
    if outside {
        json!({ "uri": dvi_analysis::ide::url_of(std::path::Path::new(rel)).to_string() })
    } else {
        json!({ "uri": uri_path(rel), "uriBaseId": "%SRCROOT%" })
    }
}

fn region(r: &Range) -> Value {
    // SARIF lines and columns are 1-based; columns in UTF-16 code units (as LSP's).
    json!({ "startLine": r.start.line + 1, "startColumn": r.start.character + 1, "endLine": r.end.line + 1, "endColumn": r.end.character + 1 })
}

/// SARIF 2.1.0 (what GitHub code scanning reads), paths relative to `%SRCROOT%` = `root_uri`.
pub fn sarif(items: &[Item], root_uri: &str, complete: bool, version: &str) -> Value {
    let metas: Vec<_> = rules::all().iter().map(|r| r.meta()).collect();
    let rule_index = |id: &str| metas.iter().position(|m| m.id == id);
    let rules_v: Vec<Value> = metas
        .iter()
        .map(|m| {
            let mut tags: Vec<String> = m.rulesets.iter().map(|r| r.name().to_string()).collect();
            tags.extend(m.guide.iter().map(|g| g.to_string()));
            json!({
                "id": m.id,
                "name": m.id,
                "shortDescription": { "text": m.summary },
                "fullDescription": { "text": m.why },
                "helpUri": m.docs_url(),
                "help": { "text": format!("{}\n\n{}", m.summary, m.why), "markdown": format!("**{}**\n\n{}\n\n[Rule documentation]({})", m.summary, m.why, m.docs_url()) },
                "defaultConfiguration": { "level": sarif_level(m.default_level) },
                "properties": { "tags": tags },
            })
        })
        .collect();
    let results: Vec<Value> = items
        .iter()
        .map(|x| {
            let mut r = json!({
                "ruleId": x.rule,
                "level": sarif_level(x.level),
                "message": { "text": x.message },
                "locations": [{ "physicalLocation": { "artifactLocation": artifact(&x.rel, x.outside), "region": x.range.as_ref().map(region) } }],
                "partialFingerprints": { "dvicodeLint/v1": x.fingerprint, "dvicodeLintContext/v1": x.context_fingerprint },
            });
            if let Some(i) = rule_index(x.rule) {
                r["ruleIndex"] = json!(i);
            }
            if !x.related.is_empty() {
                r["relatedLocations"] = json!(x
                    .related
                    .iter()
                    .enumerate()
                    .map(|(i, (p, rg, m))| json!({ "id": i, "message": { "text": m }, "physicalLocation": { "artifactLocation": artifact(p, std::path::Path::new(p).is_absolute()), "region": region(rg) } }))
                    .collect::<Vec<_>>());
            }
            match &x.kept {
                Kept::No => {}
                Kept::InSource(w) => r["suppressions"] = json!([{ "kind": "inSource", "justification": w }]),
                Kept::Baseline => r["suppressions"] = json!([{ "kind": "external", "justification": "listed in the lint baseline" }]),
            }
            r
        })
        .collect();
    json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": { "driver": { "name": "DVICode Lint", "version": version, "semanticVersion": version, "informationUri": "https://github.com/hexkDotCom/dvicode", "rules": rules_v } },
            "originalUriBaseIds": { "%SRCROOT%": { "uri": root_uri } },
            "columnKind": "utf16CodeUnits",
            "invocations": [{ "executionSuccessful": complete }],
            "results": results,
        }],
    })
}

fn xml(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            // Characters XML 1.0 cannot hold.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => o.push_str(&format!("&#x{:x};", 0xFFFD)),
            c => o.push(c),
        }
    }
    o
}

/// JUnit XML: one suite per rule, one case per linted file; a case fails with the findings at
/// or above `fail_on` (none fail with `never`).
pub fn junit(items: &[Item], files: &[String], fail_on: Option<Level>) -> String {
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let metas: Vec<_> = rules::all().iter().map(|r| r.meta()).collect();
    let failing = |x: &&Item| x.counts() && fail_on.is_some_and(|f| x.level >= f);
    let total_fail: usize = metas.iter().map(|m| files.iter().filter(|f| items.iter().filter(failing).any(|x| x.rule == m.id && &x.rel == *f)).count()).sum();
    s.push_str(&format!("<testsuites name=\"DVICode Lint\" tests=\"{}\" failures=\"{total_fail}\">\n", metas.len() * files.len()));
    for m in &metas {
        let mine: Vec<&Item> = items.iter().filter(failing).filter(|x| x.rule == m.id).collect();
        let fails = files.iter().filter(|f| mine.iter().any(|x| &x.rel == *f)).count();
        s.push_str(&format!("  <testsuite name=\"{}\" tests=\"{}\" failures=\"{fails}\">\n", xml(m.id), files.len()));
        for f in files {
            let here: Vec<&&Item> = mine.iter().filter(|x| &x.rel == f).collect();
            if here.is_empty() {
                s.push_str(&format!("    <testcase classname=\"{}\" name=\"{}\"/>\n", xml(m.id), xml(f)));
            } else {
                let body: Vec<String> = here
                    .iter()
                    .map(|x| {
                        let (l, c) = x.line_col();
                        format!("{f}:{l}:{c}: {}: {}", x.level.as_str(), x.message)
                    })
                    .collect();
                s.push_str(&format!(
                    "    <testcase classname=\"{}\" name=\"{}\">\n      <failure message=\"{} finding(s)\" type=\"{}\">{}</failure>\n    </testcase>\n",
                    xml(m.id),
                    xml(f),
                    here.len(),
                    xml(m.id),
                    xml(&body.join("\n"))
                ));
            }
        }
        s.push_str("  </testsuite>\n");
    }
    s.push_str("</testsuites>\n");
    s
}

/// GitHub's escaping of a workflow command's message.
fn gh_data(s: &str) -> String {
    s.replace('%', "%25").replace('\r', "%0D").replace('\n', "%0A")
}

/// GitHub's escaping of a workflow command's property.
fn gh_prop(s: &str) -> String {
    gh_data(s).replace(':', "%3A").replace(',', "%2C")
}

/// At most this many annotations per level (GitHub shows few of them on a step).
pub const GITHUB_MAX: usize = 50;

/// GitHub workflow annotations (`::warning file=...::message`), the counted findings only.
/// GitHub reads `file` from the repository root: the path relative to `base` (the working
/// directory, normally the checkout) when the file is below it, else relative to the project.
pub fn github(items: &[Item], base: Option<&std::path::Path>) -> String {
    let mut s = String::new();
    let mut shown: [usize; 3] = [0; 3];
    let mut left: [usize; 3] = [0; 3];
    for x in items.iter().filter(|x| x.counts()) {
        let (cmd, k) = match x.level {
            Level::Error => ("error", 0),
            Level::Warning => ("warning", 1),
            _ => ("notice", 2),
        };
        if shown[k] >= GITHUB_MAX {
            left[k] += 1;
            continue;
        }
        shown[k] += 1;
        let file = base
            .and_then(|b| std::path::Path::new(&x.path).strip_prefix(b).ok())
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| x.rel.clone());
        let mut props = format!("file={},title={}", gh_prop(&file), gh_prop(&x.tag()));
        if let Some(r) = x.range {
            props.push_str(&format!(",line={},col={},endLine={},endColumn={}", r.start.line + 1, r.start.character + 1, r.end.line + 1, r.end.character + 1));
        }
        s.push_str(&format!("::{cmd} {props}::{}\n", gh_data(&x.message)));
    }
    for (k, name) in ["errors", "warnings", "notices"].iter().enumerate() {
        if left[k] > 0 {
            s.push_str(&format!("::notice title=DVICode Lint::{} more {name} not annotated (see the lint output)\n", left[k]));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::Position;

    fn item(rule: &'static str, level: Level, rel: &str, msg: &str) -> Item {
        Item {
            rule,
            subcheck: None,
            level,
            message: msg.into(),
            path: format!("/p/{rel}"),
            rel: rel.into(),
            range: Some(Range::new(Position::new(4, 6), Position::new(4, 18))),
            related: Vec::new(),
            contexts: Vec::new(),
            kept: Kept::No,
            fingerprint: "abc".into(),
            context_fingerprint: "def".into(),
            outside: false,
        }
    }

    #[test]
    fn github_escaping() {
        let x = item("randomize-result-discarded", Level::Warning, "a,b:c.sv", "50% done\nnext");
        let out = github(&[x], None);
        assert_eq!(out, "::warning file=a%2Cb%3Ac.sv,title=randomize-result-discarded/LNG-10,line=5,col=7,endLine=5,endColumn=19::50%25 done%0Anext\n");
        let many: Vec<Item> = (0..GITHUB_MAX + 3).map(|_| item("randomize-result-discarded", Level::Error, "a.sv", "m")).collect();
        let out = github(&many, None);
        assert_eq!(out.lines().filter(|l| l.starts_with("::error")).count(), GITHUB_MAX);
        assert!(out.contains("3 more errors"));
    }

    #[test]
    fn sarif_shape() {
        let mut kept = item("randomize-result-discarded", Level::Warning, "a.sv", "m");
        kept.kept = Kept::Baseline;
        let v = sarif(&[item("randomize-result-discarded", Level::Warning, "a.sv", "m"), kept], "file:///p/", true, "1.0");
        let run = &v["runs"][0];
        assert_eq!(v["version"], "2.1.0");
        assert_eq!(run["results"][0]["locations"][0]["physicalLocation"]["region"]["startLine"], 5);
        assert_eq!(run["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]["uriBaseId"], "%SRCROOT%");
        assert_eq!(run["results"][1]["suppressions"][0]["kind"], "external");
        let idx = run["results"][0]["ruleIndex"].as_u64().unwrap() as usize;
        assert_eq!(run["tool"]["driver"]["rules"][idx]["id"], "randomize-result-discarded");
    }

    #[test]
    fn sarif_uris_are_encoded() {
        assert_eq!(uri_path("tb/a b#c.sv"), "tb/a%20b%23c.sv");
        assert_eq!(uri_path("tb/é.sv"), "tb/%C3%A9.sv");
    }

    #[test]
    fn junit_is_escaped_and_counts_failures() {
        let x = item("randomize-result-discarded", Level::Warning, "a&b.sv", "x < y");
        let out = junit(&[x], &["a&b.sv".to_string(), "c.sv".to_string()], Some(Level::Warning));
        assert!(out.contains("name=\"a&amp;b.sv\""));
        assert!(out.contains("x &lt; y"));
        assert!(out.contains("<testsuites name=\"DVICode Lint\""));
        assert!(out.contains("failures=\"1\""));
        let none = junit(&[item("randomize-result-discarded", Level::Warning, "a.sv", "m")], &["a.sv".to_string()], None);
        assert!(!none.contains("<failure"));
    }
}
