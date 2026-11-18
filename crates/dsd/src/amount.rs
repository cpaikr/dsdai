//! Cell typing for the xlsx export: which table texts become numbers.
//!
//! The rule is display fidelity. A text becomes a number only when the
//! number, shown with the Excel number format chosen here, reproduces the
//! text exactly apart from surrounding whitespace. Anything else stays text,
//! so nothing a reader sees in DART changes in the workbook.

/// A number and the Excel number format that renders it as its source text.
#[derive(Debug, Clone, PartialEq)]
pub struct Amount {
    pub value: f64,
    pub format: String,
}

/// Significant digits an IEEE double holds exactly; longer figures stay text.
const MAX_DIGITS: usize = 15;

/// The sign forms DART documents use for negative amounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Negative {
    Paren,
    Minus,
    Glyph(char),
}

/// `Some` when `text` is a figure whose Excel rendering matches it.
pub fn parse(text: &str) -> Option<Amount> {
    let t = text.trim();
    if t == "-" {
        // A nil dash: zero that still reads as a dash.
        return Some(Amount { value: 0.0, format: "#,##0;-#,##0;\"-\"".to_owned() });
    }
    let (negative, inner) = if let Some(rest) = t.strip_prefix('(') {
        (Some(Negative::Paren), rest.strip_suffix(')')?)
    } else if let Some(rest) = t.strip_prefix('-') {
        (Some(Negative::Minus), rest)
    } else if let Some(rest) = t.strip_prefix(['△', '▲']) {
        (Some(Negative::Glyph(t.chars().next().expect("prefix matched"))), rest)
    } else {
        (None, t)
    };
    let inner = inner.trim();
    let (body, percent) = match inner.strip_suffix('%') {
        Some(b) => (b, true),
        None => (inner, false),
    };
    let number = Body::parse(body)?;
    let mut value = number.value(percent)?;
    if negative.is_some() {
        // A negative zero renders as "0" or "0%", never as its source.
        if value == 0.0 {
            return None;
        }
        value = -value;
    }
    let positive = number.format(percent);
    let format = match negative {
        None => positive,
        Some(Negative::Paren) => format!("{positive};({positive})"),
        Some(Negative::Minus) => format!("{positive};-{positive}"),
        Some(Negative::Glyph(g)) => format!("{positive};\"{g}\"{positive}"),
    };
    Some(Amount { value, format })
}

/// Digits of a figure: `1,234` or `1234`, optionally with `.5` decimals.
struct Body<'a> {
    int: &'a str,
    frac: Option<&'a str>,
    grouped: bool,
}

impl<'a> Body<'a> {
    fn parse(body: &'a str) -> Option<Self> {
        let (int_part, frac) = match body.split_once('.') {
            Some((i, f)) => (i, Some(f)),
            None => (body, None),
        };
        if frac.is_some_and(|f| f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit())) {
            return None;
        }
        let groups: Vec<&str> = int_part.split(',').collect();
        let grouped = groups.len() > 1;
        let first = groups[0];
        if first.is_empty() || !first.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if grouped {
            // Thousands grouping: 1-3 leading digits, then groups of exactly three.
            if first.len() > 3 || first.starts_with('0') {
                return None;
            }
            if !groups[1..].iter().all(|g| g.len() == 3 && g.bytes().all(|b| b.is_ascii_digit())) {
                return None;
            }
        } else if first.len() > 1 && first.starts_with('0') {
            return None;
        }
        let body = Self { int: int_part, frac, grouped };
        let digits: String = body.digits();
        let significant = digits.trim_start_matches('0').len();
        if significant > MAX_DIGITS {
            return None;
        }
        Some(body)
    }

    fn digits(&self) -> String {
        let mut d: String = self.int.chars().filter(char::is_ascii_digit).collect();
        if let Some(f) = self.frac {
            d.push_str(f);
        }
        d
    }

    fn decimals(&self) -> usize {
        self.frac.map_or(0, str::len)
    }

    /// The value, read from the decimal string so it is correctly rounded;
    /// a percentage moves the decimal point two places instead of dividing.
    fn value(&self, percent: bool) -> Option<f64> {
        let digits = self.digits();
        let point = digits.len() - self.decimals();
        let shift = if percent { 2 } else { 0 };
        let text = if point >= shift {
            format!("{}.{}", &digits[..point - shift], &digits[point - shift..])
        } else {
            format!("0.{}{digits}", "0".repeat(shift - point))
        };
        let text = if text.starts_with('.') { format!("0{text}") } else { text };
        text.parse::<f64>().ok()
    }

    fn format(&self, percent: bool) -> String {
        let mut f = if self.grouped { "#,##0".to_owned() } else { "0".to_owned() };
        if self.decimals() > 0 {
            f.push('.');
            f.push_str(&"0".repeat(self.decimals()));
        }
        if percent {
            f.push('%');
        }
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(text: &str) -> (f64, String) {
        let a = parse(text).unwrap_or_else(|| panic!("{text:?} should be a number"));
        (a.value, a.format)
    }

    #[test]
    fn grouped_and_plain_integers() {
        assert_eq!(num("1,234,567"), (1_234_567.0, "#,##0".into()));
        assert_eq!(num("1234"), (1234.0, "0".into()));
        assert_eq!(num("2025"), (2025.0, "0".into()));
        assert_eq!(num("0"), (0.0, "0".into()));
        assert_eq!(num("  12,000 "), (12_000.0, "#,##0".into()), "surrounding whitespace is ignored");
        assert_eq!(num("\u{3000}5"), (5.0, "0".into()));
    }

    #[test]
    fn negative_forms_keep_their_sign() {
        assert_eq!(num("(1,234)"), (-1234.0, "#,##0;(#,##0)".into()));
        assert_eq!(num("( 1,234 )"), (-1234.0, "#,##0;(#,##0)".into()));
        assert_eq!(num("-1,234"), (-1234.0, "#,##0;-#,##0".into()));
        assert_eq!(num("△1,234"), (-1234.0, "#,##0;\"△\"#,##0".into()));
        assert_eq!(num("▲ 1,234"), (-1234.0, "#,##0;\"▲\"#,##0".into()));
        assert_eq!(num("(12)"), (-12.0, "0;(0)".into()));
    }

    #[test]
    fn decimals_and_percentages() {
        assert_eq!(num("12.50"), (12.5, "0.00".into()));
        assert_eq!(num("1,234.5"), (1234.5, "#,##0.0".into()));
        assert_eq!(num("0.125"), (0.125, "0.000".into()));
        assert_eq!(num("12.5%"), (0.125, "0.0%".into()));
        assert_eq!(num("60.00%"), (0.6, "0.00%".into()));
        assert_eq!(num("100%"), (1.0, "0%".into()));
        assert_eq!(num("0.5%"), (0.005, "0.0%".into()));
        assert_eq!(num("(3.2%)"), (-0.032, "0.0%;(0.0%)".into()));
    }

    #[test]
    fn nil_dash_is_zero_shown_as_dash() {
        assert_eq!(num("-"), (0.0, "#,##0;-#,##0;\"-\"".into()));
        assert_eq!(num(" - "), (0.0, "#,##0;-#,##0;\"-\"".into()));
    }

    #[test]
    fn refusals_stay_text() {
        for text in [
            "",
            "5,6,32",           // a list of note numbers, not a grouped figure
            "1,23",             // short group
            "1234,567",         // leading group too long
            "0,123",            // grouping after a zero
            "0123",             // leading zero
            "00",               // leading zero
            "1234567890123456", // 16 significant digits
            "+1,234",           // explicit plus
            "1,234 (*1)",       // footnote marker
            "(*1) 1,234",
            "2025.12.31", // date
            "2025-12-31",
            "1 234",  // internal space
            "(1,234", // unbalanced
            "1,234)",
            "(0)", // negative zero renders as 0
            "-0",
            "1.", // trailing point
            ".5", // no integer part
            "12.5 %",
            "%",
            "--",
            "−1,234", // U+2212 is not a sign DART uses
            "1,234원",
            "제5기",
        ] {
            assert_eq!(parse(text), None, "{text:?} must stay text");
        }
    }

    #[test]
    fn fifteen_digits_are_exact() {
        assert_eq!(num("999,999,999,999,999"), (999_999_999_999_999.0, "#,##0".into()));
        assert_eq!(num("0.000000000000001").0.to_bits(), 1e-15_f64.to_bits());
    }
}
