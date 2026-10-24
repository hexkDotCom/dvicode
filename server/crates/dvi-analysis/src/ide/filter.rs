//! The text filter of the DVICode views (Design Hierarchy, Verification Hierarchy, Compiled
//! Files). One definition, used for all three:
//!
//! - Case is ignored; spaces around the text are ignored.
//! - Text without `*` or `?` matches anywhere: `fifo` finds `tb.dut.u_rx_fifo`.
//! - `*` is any text within one level of a path (it does not cross `.` in a hierarchy path or `/`
//!   in a file path), `**` is any text, `?` is one character other than the separator. A pattern
//!   with wildcards also matches anywhere: `rx*fifo` finds `tb.u_rx_big_fifo0`.
//! - `[` and `]` are ordinary characters: `agents[2]` finds `env.agents[2]`.

/// A compiled filter.
#[derive(Clone, Debug)]
pub struct Filter {
    text: String,
    parts: Vec<Part>,
    wild: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Char(char),
    /// `?`
    One,
    /// `*`
    Star,
    /// `**`
    Stars,
}

/// Patterns longer than this are cut (the matcher keeps one bit per pattern position).
const MAX_PARTS: usize = 63;

impl Filter {
    /// `None` for an empty filter (everything matches).
    pub fn new(text: &str) -> Option<Filter> {
        let text = text.trim().to_lowercase();
        if text.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            let p = match c {
                '*' if chars.peek() == Some(&'*') => {
                    while chars.peek() == Some(&'*') {
                        chars.next();
                    }
                    Part::Stars
                }
                '*' => Part::Star,
                '?' => Part::One,
                c => Part::Char(c),
            };
            parts.push(p);
        }
        parts.truncate(MAX_PARTS);
        let wild = parts.iter().any(|p| !matches!(p, Part::Char(_)));
        Some(Filter { text, parts, wild })
    }

    /// Whether `text` matches; `sep` separates the levels of a path (`.` or `/`).
    /// The filter names an index (`g[0]`).
    pub fn has_index(&self) -> bool {
        self.text.contains('[')
    }

    pub fn matches(&self, text: &str, sep: char) -> bool {
        if !self.wild {
            return contains_ignore_case(text, &self.text);
        }
        // A set of pattern positions (bit k: the first k parts matched), advanced one character
        // at a time; a match may start at any character.
        let n = self.parts.len();
        let done = 1u64 << n;
        let close = |mut s: u64| {
            // Stars match nothing too.
            for (k, p) in self.parts.iter().enumerate() {
                if s & (1 << k) != 0 && matches!(p, Part::Star | Part::Stars) {
                    s |= 1 << (k + 1);
                }
            }
            s
        };
        let mut s = close(1);
        if s & done != 0 {
            return true;
        }
        for c in text.chars().flat_map(char::to_lowercase) {
            let mut next = 0u64;
            for (k, p) in self.parts.iter().enumerate() {
                if s & (1 << k) == 0 {
                    continue;
                }
                match *p {
                    Part::Char(x) if x == c => next |= 1 << (k + 1),
                    Part::One if c != sep => next |= 1 << (k + 1),
                    Part::Star if c != sep => next |= 1 << k,
                    Part::Stars => next |= 1 << k,
                    _ => {}
                }
            }
            s = close(next | 1);
            if s & done != 0 {
                return true;
            }
        }
        false
    }
}

fn contains_ignore_case(text: &str, lower_needle: &str) -> bool {
    if text.is_ascii() && lower_needle.is_ascii() {
        let (t, n) = (text.as_bytes(), lower_needle.as_bytes());
        return n.len() <= t.len() && t.windows(n.len()).any(|w| w.iter().zip(n).all(|(a, b)| a.to_ascii_lowercase() == *b));
    }
    text.to_lowercase().contains(lower_needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (filter, text, separator, matches): the cases the views' documentation describes.
    pub const CASES: &[(&str, &str, char, bool)] = &[
        ("fifo", "tb.dut.u_rx_fifo", '.', true),
        ("FIFO", "tb.dut.u_rx_fifo", '.', true),
        ("  fifo ", "tb.dut.u_rx_fifo", '.', true),
        ("fifo", "tb.dut.u_rx_fi_fo", '.', false),
        ("rx*fifo", "tb.u_rx_big_fifo0", '.', true),
        ("rx*fifo", "tb.u_rx.big_fifo0", '.', false),
        ("rx**fifo", "tb.u_rx.big_fifo0", '.', true),
        ("dut.*.mem", "tb.dut.u_fifo.mem", '.', true),
        ("dut.*.mem", "tb.dut.a.b.mem", '.', false),
        ("dut.**.mem", "tb.dut.a.b.mem", '.', true),
        ("u_?ifo", "tb.u_fifo", '.', true),
        ("u_?ifo", "tb.u_.ifo", '.', false),
        ("agents[2]", "uvm_test_top.env.agents[2].drv", '.', true),
        ("agents[2]", "uvm_test_top.env.agents[3]", '.', false),
        ("*", "anything", '.', true),
        ("rtl/*.sv", "/p/rtl/top.sv", '/', true),
        ("rtl/*.sv", "/p/rtl/sub/top.sv", '/', false),
        ("rtl/**.sv", "/p/rtl/sub/top.sv", '/', true),
        ("été", "Été", '.', true),
    ];

    #[test]
    fn filter_cases() {
        for (f, text, sep, want) in CASES {
            assert_eq!(Filter::new(f).unwrap().matches(text, *sep), *want, "{f:?} on {text:?}");
        }
        assert!(Filter::new("   ").is_none());
    }

    #[test]
    fn long_patterns_and_texts() {
        let f = Filter::new(&"a*".repeat(100)).unwrap();
        assert!(f.matches(&"a".repeat(1000), '.'));
        let f = Filter::new("x**y").unwrap();
        assert!(!f.matches(&"x".repeat(100_000), '.'));
    }
}
