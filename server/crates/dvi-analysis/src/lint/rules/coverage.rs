//! `coverage-weight-goal` (style guide COV-7).

use super::hexk_files::zero;
use super::util::reference_at;
use crate::lint::{Cx, Level, Rule, RuleMeta, Ruleset};
use dvi_syntax::ast::DeclKind;
use dvi_syntax::{Span, TokKind, Token};

pub struct WeightGoal;

static META: RuleMeta = RuleMeta {
    id: "coverage-weight-goal",
    guide: &["COV-7"],
    rulesets: &[Ruleset::Hexk],
    default_level: Level::Warning,
    subchecks: &[],
    summary: "`option.weight` or `option.goal` is set.",
    why: "A goal under 100 hides holes, and weights turn the total into a negotiation (COV-7). Weight 0 on a coverpoint that only builds a cross is the exception.",
    params: &[],
    text_only: false,
};

/// Where an `option` assignment is.
#[derive(PartialEq)]
enum Place {
    /// With the coverpoint's name (its label, else the variable it covers), and, for one named
    /// through a handle, whether a cross of its covergroup names it.
    Coverpoint(Option<String>, Option<bool>),
    Cross,
    Covergroup,
}

/// The value of an integer literal (`100`, `32'd100`, `'h64`, `8'b0110_0100`); `None` for one
/// with x or z digits, a real, or one too large.
fn int_value(text: &str) -> Option<u64> {
    let (radix, digits) = match text.find('\'') {
        Some(q) => {
            let rest = text[q + 1..].trim_start_matches(['s', 'S']);
            let radix = match rest.chars().next()?.to_ascii_lowercase() {
                'd' => 10,
                'h' => 16,
                'o' => 8,
                'b' => 2,
                _ => return None,
            };
            (radix, rest.get(1..)?)
        }
        None => (10, text),
    };
    let d: String = digits.chars().filter(|c| *c != '_' && !c.is_whitespace()).collect();
    let v = u64::from_str_radix(&d, radix).ok()?;
    // A sized literal keeps its low bits only (`6'd100` is 36).
    match text.find('\'').map(|q| text[..q].trim()).filter(|w| !w.is_empty()).map(|w| w.replace('_', "").parse::<u32>()) {
        Some(Ok(w)) if w < 64 => Some(v & ((1u64 << w) - 1)),
        Some(Err(_)) => None,
        _ => Some(v),
    }
}

/// Whether a `cross` of the covergroup `t[lo..hi]` names the coverpoint `name`.
fn crossed(t: &[Token], lo: usize, hi: usize, name: &str) -> bool {
    (lo..hi).filter(|&j| t[j].is_kw("cross")).any(|j| t[j + 1..hi].iter().take_while(|x| !x.is_punct(";") && !x.is_punct("{")).any(|x| x.kind == TokKind::Ident && x.text == name))
}

/// The place of the `option` at `t[k]`, which is inside the covergroup `t[lo..k]` starts: the
/// coverpoint or cross whose `{ }` holds it, else the covergroup.
fn place(t: &[Token], lo: usize, k: usize) -> Place {
    // The `{` around it (within the covergroup).
    let mut depth = 0i32;
    let mut j = k;
    while j > lo {
        j -= 1;
        let x = &t[j];
        if x.is_punct("}") {
            depth += 1;
        } else if x.is_punct("{") {
            if depth > 0 {
                depth -= 1;
                continue;
            }
            // What opens this block: scan back to the item's start, over balanced groups
            // (`iff (mode inside {A, B})`).
            let mut p = j;
            let mut nest = 0i32;
            while p > lo {
                p -= 1;
                let y = &t[p];
                if y.is_punct(")") || y.is_punct("}") || y.is_punct("]") {
                    nest += 1;
                } else if y.is_punct("(") || y.is_punct("{") || y.is_punct("[") {
                    if nest == 0 {
                        break;
                    }
                    nest -= 1;
                } else if nest == 0 {
                    if y.is_punct(";") {
                        break;
                    }
                    if y.is_kw("coverpoint") {
                        let label = (p >= 2 && t[p - 1].is_punct(":") && t[p - 2].kind == TokKind::Ident).then(|| t[p - 2].text.to_string());
                        let var = t.get(p + 1).filter(|x| x.kind == TokKind::Ident && t.get(p + 2).is_some_and(|n| n.is_punct("{") || n.is_kw("iff"))).map(|x| x.text.to_string());
                        return Place::Coverpoint(label.or(var), None);
                    }
                    if y.is_kw("cross") {
                        return Place::Cross;
                    }
                }
            }
            return Place::Covergroup;
        }
    }
    Place::Covergroup
}

impl Rule for WeightGoal {
    fn meta(&self) -> &'static RuleMeta {
        &META
    }
    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let facts = cx.facts.clone();
        let t = &facts.tokens;
        let u = a.unit(cx.unit).clone();
        // The covergroups written in this file: token ranges.
        let groups: Vec<(usize, usize)> = u
            .parsed
            .decls
            .iter()
            .filter(|d| d.kind == DeclKind::Covergroup && d.span.file == cx.file)
            .map(|d| (t.partition_point(|x| x.span.start < d.span.start), t.partition_point(|x| x.span.start < d.span.end)))
            .collect();
        for k in 0..t.len().saturating_sub(3) {
            if !(t[k].text == "option" && t[k + 1].is_punct(".") && matches!(t[k + 2].text.as_str(), "weight" | "goal") && t[k + 3].is_punct("=")) || facts.in_define(k) {
                continue;
            }
            if !cx.step(1) {
                return;
            }
            let what = t[k + 2].text.clone();
            let value: Vec<&Token> = t[k + 4..].iter().take_while(|x| !x.is_punct(";")).collect();
            let literal = |n: u64| value.len() == 1 && value[0].kind == TokKind::Number && int_value(&value[0].text) == Some(n);
            // Writing the default (weight 1, goal 100) changes nothing.
            if what == "weight" && literal(1) || what == "goal" && literal(100) {
                continue;
            }
            let is_zero = value.len() == 1 && value[0].kind == TokKind::Number && zero(&value[0].text) == Some(true);
            let place = if k > 0 && t[k - 1].is_punct(".") {
                // Through a handle: `cg.option.weight`, `cg.cp.option.weight`. What the handle
                // names: a coverpoint, a cross or a covergroup (anything else is not coverage).
                let Some(r) = k.checked_sub(2).and_then(|p| reference_at(cx, t[p].span)).and_then(|r| r.3) else {
                    cx.unknown_at("unresolved coverage handle", t[k].span);
                    continue;
                };
                let ty = a.type_of(r).map(|ty| if a.decl(ty).kind == DeclKind::Typedef { a.follow_typedef(ty).unwrap_or(ty) } else { ty });
                match (a.decl(r).kind, ty.map(|ty| a.decl(ty).kind)) {
                    (DeclKind::Coverpoint, _) => {
                        // Its covergroup may be written in another file.
                        let name = a.decl(r).name.to_string();
                        let crossed_there = a.decl(r).parent.map(|g| {
                            let g = a.decl(crate::world::DeclRef { unit: r.unit, decl: g });
                            let ff = a.lint_facts.file(a, r.unit, g.span.file);
                            let (lo, hi) = (ff.tokens.partition_point(|x| x.span.start < g.span.start), ff.tokens.partition_point(|x| x.span.start < g.span.end));
                            crossed(&ff.tokens, lo, hi, &name)
                        });
                        Place::Coverpoint(Some(name), crossed_there)
                    }
                    (DeclKind::Cross, _) => Place::Cross,
                    (_, Some(DeclKind::Covergroup)) | (DeclKind::Covergroup, _) => Place::Covergroup,
                    _ => continue,
                }
            } else {
                // Inside a covergroup (an `option` of anything else is not coverage).
                let Some(&(lo, _)) = groups.iter().find(|(lo, hi)| *lo <= k && k < *hi) else { continue };
                place(t, lo, k)
            };
            // COV-7's exception: weight 0 on a coverpoint that only builds a cross (one of
            // this file's covergroups crosses it).
            if what == "weight" && is_zero {
                if let Place::Coverpoint(name, crossed_there) = &place {
                    let Some(name) = name else {
                        cx.unknown_at("coverpoint without a name", t[k].span);
                        continue;
                    };
                    let here = || groups.iter().any(|(lo, hi)| *lo <= k && k < *hi && crossed(t, *lo, *hi, name));
                    if crossed_there.unwrap_or_else(here) {
                        continue;
                    }
                }
            }
            let span = Span::new(cx.file, t[k].span.start, t[k + 2].span.end);
            cx.report(None, span, format!("option.{what} is set: keep the default ({})", if what == "goal" { "100" } else { "1" }), Vec::new());
        }
    }
}
