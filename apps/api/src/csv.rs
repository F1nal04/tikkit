//! CSV output for Admin exports (RFC 4180), safe to open in a spreadsheet.
//! Every cell is quoted; a cell a spreadsheet would evaluate as a formula is
//! prefixed with an apostrophe so it stays text.

/// One quoted cell. Formula markers, tabs and line breaks are neutralized
/// even after leading whitespace or quotes a spreadsheet may ignore.
pub fn cell(value: &str) -> String {
    let leading = value.trim_start_matches(|c: char| {
        // Tabs and line breaks are dangerous prefixes themselves.
        !matches!(c, '\t' | '\r' | '\n')
            && (c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '\u{feff}'))
    });
    let formula =
        leading.starts_with(['=', '+', '-', '@', '\t', '\r', '\n', '＝', '＋', '－', '＠']);
    let mut out = String::with_capacity(value.len() + 3);
    out.push('"');
    if formula {
        out.push('\'');
    }
    out.push_str(&value.replace('"', "\"\""));
    out.push('"');
    out
}

/// One record, terminated by CRLF.
pub fn line<T: AsRef<str>>(cells: impl IntoIterator<Item = T>) -> String {
    let cells: Vec<String> = cells.into_iter().map(|c| cell(c.as_ref())).collect();
    format!("{}\r\n", cells.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formulas_are_neutralized_and_quotes_escaped() {
        assert_eq!(cell("plain"), "\"plain\"");
        assert_eq!(cell("=HYPERLINK(\"x\")"), "\"'=HYPERLINK(\"\"x\"\")\"");
        for start in ["+1", "-1", "@SUM(A1)", "\tx", "\rx"] {
            assert!(cell(start).starts_with("\"'"), "{start:?}");
        }
        for prefix in ["  ", "  \" ' ", "\u{feff}"] {
            for start in ['=', '+', '-', '@', '\t', '\r', '\n', '＝', '＋', '－', '＠'] {
                let input = format!("{prefix}{start}formula");
                assert!(cell(&input).starts_with("\"'"), "{input:?}");
            }
        }
        assert_eq!(cell("a,b\nc"), "\"a,b\nc\"");
        assert_eq!(line(["a", "=b"]), "\"a\",\"'=b\"\r\n");
    }
}
