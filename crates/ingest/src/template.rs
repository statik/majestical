//! `{token}` layout templates for the destination path inside a PARA node.
use crate::IngestError;

/// Values substituted into a layout template.
pub struct TemplateCtx {
    pub date: String,
    pub source_label: String,
}

/// Renders `{token}` templates. Tokens: `{date}`, `{source-label}`. The
/// result is a relative path fragment; values must not smuggle separators
/// or traversal segments into it.
///
/// # Errors
/// Returns `IngestError::Template` on unknown tokens, unbalanced braces, or
/// values that would produce an absolute, empty, or `..` path segment.
pub fn render(template: &str, ctx: &TemplateCtx) -> Result<String, IngestError> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let literal = &rest[..open];
        if literal.contains('}') {
            return Err(IngestError::Template(format!(
                "unbalanced '}}' in template '{template}'"
            )));
        }
        out.push_str(literal);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return Err(IngestError::Template(format!(
                "unbalanced '{{' in template '{template}'"
            )));
        };
        let token = &after[..close];
        let value = match token {
            "date" => &ctx.date,
            "source-label" => &ctx.source_label,
            other => {
                return Err(IngestError::Template(format!(
                    "unknown token '{other}' — known: date, source-label"
                )));
            }
        };
        if value.contains('/') || value.contains('\\') {
            return Err(IngestError::Template(format!(
                "value for '{{{token}}}' contains a path separator: '{value}'"
            )));
        }
        out.push_str(value);
        rest = &after[close + 1..];
    }
    if rest.contains('}') {
        return Err(IngestError::Template(format!(
            "unbalanced '}}' in template '{template}'"
        )));
    }
    out.push_str(rest);
    for seg in out.split('/') {
        if seg.is_empty() || seg == ".." || seg == "." {
            return Err(IngestError::Template(format!(
                "template '{template}' rendered an unsafe segment '{seg}'"
            )));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn renders_known_tokens() {
        let ctx = TemplateCtx {
            date: "2026-07-29".into(),
            source_label: "card-a".into(),
        };
        assert_eq!(
            render("{date}/{source-label}", &ctx).expect("render"),
            "2026-07-29/card-a"
        );
    }

    #[test]
    fn unknown_token_is_an_error() {
        let ctx = TemplateCtx {
            date: "2026-07-29".into(),
            source_label: "card-a".into(),
        };
        let err = render("{nope}", &ctx).expect_err("unknown token must error");
        assert!(
            err.to_string().contains("unknown token 'nope'"),
            "got: {err}"
        );
    }

    #[test]
    fn unbalanced_brace_is_an_error() {
        let ctx = TemplateCtx {
            date: "2026-07-29".into(),
            source_label: "card-a".into(),
        };
        let err = render("{date", &ctx).expect_err("unbalanced brace must error");
        assert!(err.to_string().contains("unbalanced"), "got: {err}");
    }

    #[test]
    fn stray_close_brace_is_an_error() {
        let ctx = TemplateCtx {
            date: "2026-07-29".into(),
            source_label: "card-a".into(),
        };
        let err = render("date}", &ctx).expect_err("stray '}' must error");
        assert!(
            err.to_string().contains("unbalanced") && err.to_string().contains('}'),
            "got: {err}"
        );
    }

    #[test]
    fn traversal_and_absolute_segments_are_rejected() {
        let ctx = TemplateCtx {
            date: "..".into(),
            source_label: "card-a".into(),
        };
        render("{date}/{source-label}", &ctx).expect_err("traversal date must error");

        let ctx = TemplateCtx {
            date: "2026-07-29".into(),
            source_label: "/abs".into(),
        };
        render("{date}/{source-label}", &ctx).expect_err("separator in value must error");
    }

    /// The existing traversal test's "/abs" value contains a forward slash,
    /// so it also produces an empty path segment once rendered (the final
    /// per-segment safety check catches it too) — that leaves the value's
    /// own `contains('/') || contains('\\')` check undiscriminated from a
    /// `||` -> `&&` mutation. A value with *only* a backslash renders with
    /// no empty segment (nothing else here would catch it), so this value
    /// must be rejected by that check directly.
    #[test]
    fn a_backslash_only_value_is_rejected_even_without_a_forward_slash() {
        let ctx = TemplateCtx {
            date: "2026-07-29".into(),
            source_label: "a\\b".into(),
        };
        render("{date}/{source-label}", &ctx).expect_err("backslash in value must error");
    }

    proptest! {
        #[test]
        fn safe_values_render_to_safe_relative_paths(
            date in "[a-zA-Z0-9._ -]{1,12}",
            source_label in "[a-zA-Z0-9._ -]{1,12}",
        ) {
            let ctx = TemplateCtx { date, source_label };
            if let Ok(out) = render("{date}/{source-label}", &ctx) {
                prop_assert!(!out.starts_with('/'));
                for seg in out.split('/') {
                    prop_assert!(!seg.is_empty());
                    prop_assert_ne!(seg, "..");
                }
            }
        }
    }
}
