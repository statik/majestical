//! The `maj search` query language: bare terms (matched semantically and by
//! name) plus `key:value` hard filters, `-` negation, and double-quote
//! grouping. One parser, shared by every future surface (GUI omnibox, MCP).
//! Purely syntactic: key validation and filter resolution happen in the
//! search command against `majestical_core::ports::Filter`'s contracts.

use anyhow::{Result, bail};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RawFilter {
    pub key: String,
    pub value: String,
    pub negated: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ParsedQuery {
    pub terms: Vec<String>,
    pub filters: Vec<RawFilter>,
}

/// Split into whitespace-separated tokens honoring double quotes (stripped),
/// then classify: `-` prefix negates; `key:value` with a non-empty ASCII-
/// alphabetic key (case-folded) is a filter; anything else is a term.
///
/// # Errors
/// Returns an error for an unbalanced quote, an empty filter value, or a
/// `-` negation applied to a bare term.
pub(crate) fn parse_query(input: &str) -> Result<ParsedQuery> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in input.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if in_quotes {
        bail!("unbalanced quote in query: {input}");
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    let mut parsed = ParsedQuery::default();
    for token in tokens {
        let (negated, body) = match token.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, token.as_str()),
        };
        match body.split_once(':') {
            Some((key, value))
                if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphabetic()) =>
            {
                if value.is_empty() {
                    bail!("filter '{key}:' has no value");
                }
                parsed.filters.push(RawFilter {
                    key: key.to_ascii_lowercase(),
                    value: value.to_string(),
                    negated,
                });
            }
            _ => {
                if negated {
                    bail!("'-' negation only applies to key:value filters: -{body}");
                }
                parsed.terms.push(body.to_string());
            }
        }
    }
    Ok(parsed)
}

/// Whether `year` is a leap year in the proleptic Gregorian calendar.
fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Number of days in `month` (1-12) of `year`.
fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// `YYYY-MM-DD` → milliseconds since the Unix epoch at UTC midnight, via
/// Howard Hinnant's days-from-civil algorithm (no `chrono` dependency).
///
/// # Errors
/// Returns an error when the string isn't `YYYY-MM-DD`, names an impossible
/// civil date (bad month, or a day beyond that month's real length,
/// leap-year aware), predates the Unix epoch, or names a year past 9999 (a
/// bound that also keeps `days * 86_400_000` below from overflowing `i64`).
pub(crate) fn parse_date_ms(value: &str) -> Result<u64> {
    let mut parts = value.split('-');
    let (Some(y), Some(m), Some(d), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        bail!("invalid date '{value}': expected YYYY-MM-DD");
    };
    let bad = || anyhow::anyhow!("invalid date '{value}': expected YYYY-MM-DD");
    let y: i64 = y.parse().map_err(|_| bad())?;
    let m: i64 = m.parse().map_err(|_| bad())?;
    let d: i64 = d.parse().map_err(|_| bad())?;

    if !(1970..=9999).contains(&y) {
        bail!("year out of range (expected 1970-9999): '{value}'");
    }
    if !(1..=12).contains(&m) {
        bail!("invalid month in date '{value}'");
    }
    if d < 1 || d > days_in_month(y, m) {
        bail!("invalid day in date '{value}'");
    }

    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400_000).map_err(|_| anyhow::anyhow!("date out of range: '{value}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_terms_and_filters_separate() {
        let q =
            parse_query(r"golden retriever tag:pets -tag:rejected vol:Media2024").expect("parse");
        assert_eq!(q.terms, vec!["golden", "retriever"]);
        assert_eq!(
            q.filters,
            vec![
                RawFilter {
                    key: "tag".into(),
                    value: "pets".into(),
                    negated: false
                },
                RawFilter {
                    key: "tag".into(),
                    value: "rejected".into(),
                    negated: true
                },
                RawFilter {
                    key: "vol".into(),
                    value: "Media2024".into(),
                    negated: false
                },
            ]
        );
    }

    #[test]
    fn quotes_group_whitespace_in_terms_and_values() {
        let q = parse_query(r#""golden gate" tag:"family trip""#).expect("parse");
        assert_eq!(q.terms, vec!["golden gate"]);
        assert_eq!(
            q.filters,
            vec![RawFilter {
                key: "tag".into(),
                value: "family trip".into(),
                negated: false
            }]
        );
    }

    #[test]
    fn unbalanced_quote_is_a_clear_error() {
        let err = parse_query(r#"beach "sunset"#).expect_err("must fail");
        assert!(err.to_string().contains("unbalanced quote"));
    }

    #[test]
    fn empty_input_yields_empty_query() {
        let q = parse_query("   ").expect("parse");
        assert!(q.terms.is_empty() && q.filters.is_empty());
    }

    #[test]
    fn empty_filter_value_is_an_error() {
        let err = parse_query("tag:").expect_err("must fail");
        assert!(err.to_string().contains("has no value"));
    }

    #[test]
    fn negation_on_a_bare_term_is_an_error() {
        let err = parse_query("-beach").expect_err("must fail");
        assert!(err.to_string().contains("negation only applies"));
    }

    #[test]
    fn in_filter_parses_sources() {
        let parsed = parse_query("barn in:transcript in:ocr").expect("parse");
        assert_eq!(parsed.terms, vec!["barn"]);
        let sources: Vec<_> = parsed
            .filters
            .iter()
            .filter(|f| f.key == "in")
            .map(|f| f.value.as_str())
            .collect();
        assert_eq!(sources, vec!["transcript", "ocr"]);
    }

    #[test]
    fn negated_in_filter_is_rejected_at_resolve_time_not_parse_time() {
        // The parser stays generic (RawFilter); rejection happens during
        // filter resolution like before:/after: negation does.
        let parsed = parse_query("-in:ocr").expect("parse");
        assert!(parsed.filters[0].negated);
    }

    #[test]
    fn uppercase_keys_fold_and_non_alpha_colons_are_terms() {
        let q = parse_query("TAG:x 16:9 a:b:c").expect("parse");
        assert_eq!(
            q.filters[0],
            RawFilter {
                key: "tag".into(),
                value: "x".into(),
                negated: false
            }
        );
        assert!(
            q.terms.contains(&"16:9".to_string()),
            "numeric-key token stays a term"
        );
        assert_eq!(
            q.filters[1],
            RawFilter {
                key: "a".into(),
                value: "b:c".into(),
                negated: false
            },
            "split_once keeps the rest of the value intact"
        );
    }

    #[test]
    fn dates_parse_to_utc_midnight_ms() {
        assert_eq!(parse_date_ms("1970-01-02").expect("parse"), 86_400_000);
        assert_eq!(
            parse_date_ms("2026-07-30").expect("parse"),
            1_785_369_600_000
        );
        assert!(parse_date_ms("2026-13-01").is_err());
        assert!(
            parse_date_ms("2026-02-30").is_err(),
            "impossible civil date"
        );
        assert!(parse_date_ms("not-a-date").is_err());
        assert!(parse_date_ms("1969-12-31").is_err(), "pre-epoch rejected");
        assert!(
            parse_date_ms("300000000000-01-01").is_err(),
            "an absurdly large year must be rejected, not overflow the days*ms multiply"
        );
    }
}
