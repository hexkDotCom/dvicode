//! Jinja templates in argument files, as some run flows write them:
//!
//! ```text
//! {% if "gate_sim" in fl_switches %} gate.f {% elif "x" not in fl_switches %} rtl.f {% else %} stub.f {% endif %}
//! ```
//!
//! Conditions are evaluated against the switches given with `+dvicode_switches+` (a plain
//! variable name such as `fl_switches` stands for that list). The rendered text keeps the byte
//! offsets of the original: tags and the text of false branches become spaces, newlines stay.

/// Whether `text` may hold Jinja tags.
pub fn has_tags(text: &str) -> bool {
    text.contains("{%") || text.contains("{{") || text.contains("{#")
}

pub struct Rendered {
    pub text: String,
    /// Problems: byte range and message.
    pub problems: Vec<(u32, u32, String)>,
    /// What each condition evaluated to: byte range of the tag and text.
    pub notes: Vec<(u32, u32, String)>,
}

struct Branch {
    /// Whether the text around the `if` is kept.
    outer: bool,
    /// A branch of this `if` was already taken.
    taken: bool,
    active: bool,
    start: (u32, u32),
}

pub fn render(text: &str, switches: &[String]) -> Rendered {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let mut problems = Vec::new();
    let mut notes = Vec::new();
    let mut stack: Vec<Branch> = Vec::new();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for c in &mut out[from..to] {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    };
    let shown = if switches.is_empty() { "none".to_string() } else { switches.join(", ") };
    let mut i = 0;
    let mut copied_from = 0;
    while i + 1 < b.len() {
        let close: &[u8] = match (b[i], b[i + 1]) {
            (b'{', b'%') => b"%}",
            (b'{', b'{') => b"}}",
            (b'{', b'#') => b"#}",
            _ => {
                i += 1;
                continue;
            }
        };
        let active = stack.last().map_or(true, |f| f.active);
        if !active {
            blank(&mut out, copied_from, i);
        }
        let start = i;
        let Some(len) = b[i + 2..].windows(2).position(|w| w == close) else {
            problems.push((start as u32, (start + 2) as u32, "Unterminated Jinja tag".to_string()));
            blank(&mut out, start, b.len());
            copied_from = b.len();
            break;
        };
        let end = i + 2 + len + 2;
        let span = (start as u32, end as u32);
        blank(&mut out, start, end);
        i = end;
        copied_from = end;
        let inner = text[start + 2..end - 2].trim_matches(|c: char| c == '-' || c == '+' || c == '~').trim();
        match close {
            b"}}" => {
                if active {
                    problems.push((span.0, span.1, format!("Jinja expression {{{{ {inner} }}}} is not supported; it is left out")));
                }
                continue;
            }
            b"#}" => continue,
            _ => {}
        }
        let (kw, rest) = inner.split_once(char::is_whitespace).unwrap_or((inner, ""));
        let rest = rest.trim();
        match kw {
            "if" => {
                let v = active && eval_reported(rest, switches, span, &mut problems);
                if active {
                    notes.push((span.0, span.1, format!("**Jinja condition** `{rest}` is {v} (switches: {shown})")));
                }
                stack.push(Branch { outer: active, taken: v, active: v, start: span });
            }
            "elif" => match stack.last_mut() {
                Some(f) => {
                    let v = f.outer && !f.taken && eval_reported(rest, switches, span, &mut problems);
                    if f.outer && !f.taken {
                        notes.push((span.0, span.1, format!("**Jinja condition** `{rest}` is {v} (switches: {shown})")));
                    }
                    f.active = v;
                    f.taken |= v;
                }
                None => problems.push((span.0, span.1, "{% elif %} without {% if %}".to_string())),
            },
            "else" => match stack.last_mut() {
                Some(f) => {
                    f.active = f.outer && !f.taken;
                    f.taken = true;
                }
                None => problems.push((span.0, span.1, "{% else %} without {% if %}".to_string())),
            },
            "endif" => {
                if stack.pop().is_none() {
                    problems.push((span.0, span.1, "{% endif %} without {% if %}".to_string()));
                }
            }
            _ if active => problems.push((span.0, span.1, format!("Jinja tag {{% {kw} %}} is not supported; it is ignored"))),
            _ => {}
        }
    }
    if stack.last().is_some_and(|f| !f.active) {
        blank(&mut out, copied_from, b.len());
    }
    for f in stack {
        problems.push((f.start.0, f.start.1, "{% if %} without {% endif %}".to_string()));
    }
    Rendered { text: String::from_utf8(out).unwrap_or_else(|_| text.to_string()), problems, notes }
}

fn eval_reported(expr: &str, switches: &[String], span: (u32, u32), problems: &mut Vec<(u32, u32, String)>) -> bool {
    let toks = tokenize(expr);
    let mut p = Eval { toks: &toks, pos: 0, switches };
    let v = p.or();
    if p.pos < toks.len() || v.is_none() {
        problems.push((span.0, span.1, format!("Cannot evaluate the Jinja condition `{expr}`; taken as false")));
        return false;
    }
    v.unwrap_or(false)
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Str(String),
    Name(String),
    Punct(char),
    Other,
}

fn tokenize(s: &str) -> Vec<Tok> {
    let c: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '"' || ch == '\'' {
            let mut j = i + 1;
            let mut v = String::new();
            while j < c.len() && c[j] != ch {
                if c[j] == '\\' && j + 1 < c.len() {
                    j += 1;
                }
                v.push(c[j]);
                j += 1;
            }
            out.push(Tok::Str(v));
            i = j + 1;
        } else if ch.is_alphanumeric() || ch == '_' {
            let mut j = i;
            while j < c.len() && (c[j].is_alphanumeric() || c[j] == '_') {
                j += 1;
            }
            out.push(Tok::Name(c[i..j].iter().collect()));
            i = j;
        } else if matches!(ch, '(' | ')' | '[' | ']' | '.') {
            out.push(Tok::Punct(ch));
            i += 1;
        } else {
            out.push(Tok::Other);
            i += 1;
        }
    }
    out
}

/// A value; `None` where it cannot be known (a subscript, an unsupported operator).
#[derive(Clone, Debug)]
enum Val {
    Str(String),
    Switches,
    Bool(bool),
}

struct Eval<'a> {
    toks: &'a [Tok],
    pos: usize,
    switches: &'a [String],
}

impl Eval<'_> {
    fn peek_name(&self, n: &str) -> bool {
        matches!(self.toks.get(self.pos), Some(Tok::Name(x)) if x == n)
    }

    fn truth(&self, v: Option<Val>) -> Option<bool> {
        match v? {
            Val::Bool(b) => Some(b),
            Val::Str(s) => Some(!s.is_empty()),
            Val::Switches => Some(!self.switches.is_empty()),
        }
    }

    fn or(&mut self) -> Option<bool> {
        let mut v = self.and();
        while self.peek_name("or") {
            self.pos += 1;
            let r = self.and();
            v = match (v, r) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            };
        }
        v
    }

    fn and(&mut self) -> Option<bool> {
        let mut v = self.not();
        while self.peek_name("and") {
            self.pos += 1;
            let r = self.not();
            v = match (v, r) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            };
        }
        v
    }

    fn not(&mut self) -> Option<bool> {
        if self.peek_name("not") {
            self.pos += 1;
            return self.not().map(|b| !b);
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Option<bool> {
        // A parenthesized condition is a truth value already.
        let lhs = self.primary();
        let negate = if self.peek_name("in") {
            self.pos += 1;
            false
        } else if self.peek_name("not") && matches!(self.toks.get(self.pos + 1), Some(Tok::Name(x)) if x == "in") {
            self.pos += 2;
            true
        } else {
            return self.truth(lhs);
        };
        let rhs = self.primary();
        let v = match (lhs?, rhs?) {
            (Val::Str(s), Val::Switches) => self.switches.iter().any(|x| *x == s),
            (Val::Str(s), Val::Str(t)) => t.contains(&s),
            _ => return None,
        };
        Some(v != negate)
    }

    fn primary(&mut self) -> Option<Val> {
        let t = self.toks.get(self.pos)?.clone();
        self.pos += 1;
        let v = match t {
            Tok::Punct('(') => {
                let v = self.or();
                if self.toks.get(self.pos) == Some(&Tok::Punct(')')) {
                    self.pos += 1;
                }
                v.map(Val::Bool)
            }
            Tok::Str(s) => Some(Val::Str(s)),
            Tok::Name(n) => match n.as_str() {
                "true" | "True" => Some(Val::Bool(true)),
                "false" | "False" | "none" | "None" => Some(Val::Bool(false)),
                _ => Some(Val::Switches),
            },
            _ => None,
        };
        // `x[0]['config']`, `x.attr`: not known.
        let mut v = v;
        while let Some(Tok::Punct(p @ ('[' | '.'))) = self.toks.get(self.pos) {
            v = None;
            if *p == '.' {
                self.pos += 2;
                continue;
            }
            let mut depth = 0;
            while let Some(t) = self.toks.get(self.pos) {
                self.pos += 1;
                match t {
                    Tok::Punct('[') => depth += 1,
                    Tok::Punct(']') => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(text: &str, sw: &[&str]) -> (Vec<String>, usize) {
        let sw: Vec<String> = sw.iter().map(|s| s.to_string()).collect();
        let r = render(text, &sw);
        assert_eq!(r.text.len(), text.len());
        assert_eq!(r.text.matches('\n').count(), text.matches('\n').count());
        (r.text.split_whitespace().map(str::to_string).collect(), r.problems.len())
    }

    #[test]
    fn branches_follow_the_switches() {
        let t = "a.sv\n{% if \"gate\" in fl_switches %}\ng.sv\n{% elif \"stub\" not in fl_switches %}\nrtl.sv\n{% else %}\nstub.sv\n{% endif %}\nz.sv\n";
        assert_eq!(kept(t, &["gate"]), (vec!["a.sv".into(), "g.sv".into(), "z.sv".into()], 0));
        assert_eq!(kept(t, &[]), (vec!["a.sv".into(), "rtl.sv".into(), "z.sv".into()], 0));
        assert_eq!(kept(t, &["stub"]), (vec!["a.sv".into(), "stub.sv".into(), "z.sv".into()], 0));
    }

    #[test]
    fn boolean_operators_and_nesting() {
        let t = "{% if (\"a\" in s) or (\"b\" in s) %}ab.sv{% if not \"c\" in s and \"d\" in s %} d.sv{% endif %}{% endif %}";
        assert_eq!(kept(t, &["b", "d"]).0, vec!["ab.sv", "d.sv"]);
        assert_eq!(kept(t, &["b", "c", "d"]).0, vec!["ab.sv"]);
        assert!(kept(t, &["d"]).0.is_empty());
    }

    #[test]
    fn unsupported_parts_are_reported() {
        // An expression, a subscript that cannot be evaluated, a missing endif.
        let (words, problems) = kept("x/{{ dirname }}/a.sv\n{% if \"x\" in sw[0]['config'] %}b.sv{% endif %}\n{% if \"y\" in sw %}c.sv\n", &["y"]);
        assert_eq!(words, vec!["x/", "/a.sv", "c.sv"]);
        assert_eq!(problems, 3);
    }
}
