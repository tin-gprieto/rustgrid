//! The parameters cell and `{{name}}` templates.
//!
//! On disk a parameters cell is a code cell tagged `parameters` (papermill's convention) holding
//! one Python assignment per line: `country = "AR"`, `year = 2025`. Jupyter runs it as Python and
//! defines the variables JupySQL expands in `{{country}}`; RustGrid reads the same lines and
//! expands the templates itself, so one file works on both sides.
//!
//! Expansion is **text replacement**, exactly like JupySQL's: the value lands in the SQL as it
//! is, with no quoting and no bound parameter, so it gives no protection against SQL injection.

use std::collections::HashMap;

/// The cell metadata tag that marks a parameters cell.
pub const PARAMETERS_TAG: &str = "parameters";

/// One `name = value` line of a parameters cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub value: ParamValue,
}

/// A parameter's Python literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamValue {
    /// A string literal, unescaped (`"AR"` is `Text("AR")`).
    Text(String),
    /// A number, `True`, `False` or `None`, verbatim.
    Literal(String),
}

impl ParamValue {
    /// What `{{name}}` expands to (Python's `str()` of the value): a string's text without its
    /// quotes, any other literal as written.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Text(text) | Self::Literal(text) => text,
        }
    }

    /// The value typed into an editor: a number, `True`, `False` or `None` stays a literal,
    /// anything else is a string.
    pub fn from_input(input: &str) -> Self {
        let input = input.trim();
        if is_number(input) || matches!(input, "True" | "False" | "None") {
            Self::Literal(input.to_string())
        } else {
            Self::Text(input.to_string())
        }
    }

    fn to_python(&self) -> String {
        match self {
            Self::Literal(literal) => literal.clone(),
            Self::Text(text) => {
                let mut out = String::with_capacity(text.len() + 2);
                out.push('"');
                for c in text.chars() {
                    match c {
                        '\\' => out.push_str("\\\\"),
                        '"' => out.push_str("\\\""),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c => out.push(c),
                    }
                }
                out.push('"');
                out
            }
        }
    }
}

/// The parameters of a cell's source: one `name = literal` per line, blank lines allowed.
/// `None` when any line is something else, so the cell stays an ordinary code cell.
pub fn parse_parameters(source: &str) -> Option<Vec<Param>> {
    source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(parse_line)
        .collect()
}

/// The cell source for `params`, one assignment per line.
pub fn format_parameters(params: &[Param]) -> String {
    params
        .iter()
        .map(|param| format!("{} = {}", param.name, param.value.to_python()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether `name` can be a parameter: a Python identifier (ASCII) that is not a keyword.
pub fn is_param_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !PYTHON_KEYWORDS.contains(&name)
}

/// A `{{name}}` whose name no parameter defines.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("undefined parameter `{0}`")]
pub struct MissingParam(pub String);

/// Expand every `{{name}}` (spaces inside the braces allowed) in `sql` with its parameter's
/// value. When a name is defined more than once the last definition wins. Braces that do not
/// hold a plain name (`{{ a.b }}`, `{{}}`) are left as written. The whole text is expanded,
/// string literals and comments included, as JupySQL does.
pub fn substitute(sql: &str, params: &[Param]) -> Result<String, MissingParam> {
    let values: HashMap<&str, &str> = params
        .iter()
        .map(|param| (param.name.as_str(), param.value.as_str()))
        .collect();
    let mut out = String::with_capacity(sql.len());
    let mut rest = sql;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            break;
        };
        let name = after[..close].trim();
        out.push_str(&rest[..open]);
        if is_param_name(name) {
            let value = values
                .get(name)
                .ok_or_else(|| MissingParam(name.to_string()))?;
            out.push_str(value);
        } else {
            out.push_str(&rest[open..open + 2 + close + 2]);
        }
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn parse_line(line: &str) -> Option<Param> {
    let (name, value) = line.split_once('=')?;
    let name = name.trim();
    if !is_param_name(name) {
        return None;
    }
    let value = value.trim();
    let value = if is_number(value) || matches!(value, "True" | "False" | "None") {
        ParamValue::Literal(value.to_string())
    } else {
        ParamValue::Text(parse_string(value)?)
    };
    Some(Param {
        name: name.to_string(),
        value,
    })
}

/// A Python number: optional sign, digits with `_` separators, a fraction, an exponent.
fn is_number(text: &str) -> bool {
    let body = text.strip_prefix(['+', '-']).unwrap_or(text);
    body.starts_with(|c: char| c.is_ascii_digit() || c == '.')
        && body
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '_' | '.' | 'e' | 'E' | '+' | '-'))
        && body.replace('_', "").parse::<f64>().is_ok()
}

/// A single- or double-quoted Python string with the common escapes; anything fancier (prefixes,
/// triple quotes, other escapes) is not a parameter value.
fn parse_string(text: &str) -> Option<String> {
    let quote = text.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let inner = text.strip_prefix(quote)?.strip_suffix(quote)?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(match chars.next()? {
                '\\' => '\\',
                '"' => '"',
                '\'' => '\'',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                _ => return None,
            }),
            c if c == quote => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

const PYTHON_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn text(name: &str, value: &str) -> Param {
        Param {
            name: name.into(),
            value: ParamValue::Text(value.into()),
        }
    }

    fn literal(name: &str, value: &str) -> Param {
        Param {
            name: name.into(),
            value: ParamValue::Literal(value.into()),
        }
    }

    #[test]
    fn parses_assignments() {
        let params =
            parse_parameters("country = \"AR\"\n\nyear=2025\nrate = -1_000.5e3\nflag = True\nx = None\ns = 'it\\'s'\n")
                .unwrap();
        assert_eq!(
            params,
            vec![
                text("country", "AR"),
                literal("year", "2025"),
                literal("rate", "-1_000.5e3"),
                literal("flag", "True"),
                literal("x", "None"),
                text("s", "it's"),
            ]
        );
        assert_eq!(parse_parameters("").unwrap(), Vec::new());
    }

    #[test]
    fn anything_else_is_not_a_parameters_cell() {
        for source in [
            "print(1)",
            "x = 1  # comment",
            "# comment",
            "x = f\"a\"",
            "x = \"a\" + \"b\"",
            "x = [1]",
            "1x = 1",
            "for = 1",
            "x = \"unterminated",
            "x = \"bad \\q escape\"",
            "x = \"a\"b\"",
            "x = inf",
            "x = 1\ny = foo",
        ] {
            assert_eq!(parse_parameters(source), None, "{source}");
        }
    }

    #[test]
    fn format_escapes_and_round_trips() {
        let params = vec![
            text("q", "say \"hi\"\\ \n\tok"),
            literal("n", "42"),
            text("empty", ""),
        ];
        let source = format_parameters(&params);
        assert_eq!(
            source,
            "q = \"say \\\"hi\\\"\\\\ \\n\\tok\"\nn = 42\nempty = \"\""
        );
        assert_eq!(parse_parameters(&source).unwrap(), params);
    }

    #[test]
    fn value_from_input() {
        assert_eq!(
            ParamValue::from_input(" 12 "),
            ParamValue::Literal("12".into())
        );
        assert_eq!(
            ParamValue::from_input("False"),
            ParamValue::Literal("False".into())
        );
        assert_eq!(ParamValue::from_input("AR"), ParamValue::Text("AR".into()));
        assert_eq!(
            ParamValue::from_input("12a"),
            ParamValue::Text("12a".into())
        );
    }

    #[test]
    fn substitutes_names_with_or_without_spaces() {
        let params = vec![text("country", "AR"), literal("year", "2025")];
        assert_eq!(
            substitute(
                "select * from t where c = '{{country}}' and y = {{ year }}",
                &params
            )
            .unwrap(),
            "select * from t where c = 'AR' and y = 2025"
        );
    }

    #[test]
    fn the_last_definition_wins() {
        let params = vec![text("c", "first"), text("c", "last")];
        assert_eq!(substitute("{{c}}", &params).unwrap(), "last");
    }

    #[test]
    fn a_missing_parameter_is_an_error() {
        assert_eq!(
            substitute("select {{a}}, {{b}}", &[literal("a", "1")]),
            Err(MissingParam("b".into()))
        );
    }

    #[test]
    fn braces_without_a_plain_name_are_kept() {
        let sql = "select '{{}}', '{{ a.b }}', '{{', '}}', json '{\"a\":{\"b\":1}}'";
        assert_eq!(substitute(sql, &[]).unwrap(), sql);
    }
}
