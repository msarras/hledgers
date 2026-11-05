//! CSV (character-separated values) data: the lexer (the cassava-megaparsec
//! parser that hledger uses).

use std::collections::BTreeSet;

use crate::parse::{Item, ParseError};

/// A CSV record: its field values.
pub type CsvRecord = Vec<String>;

/// The records in some CSV text with this field separator, or an error as
/// megaparsec shows it, named `name` (cassava-megaparsec's decodeWith,
/// without a header). Fields are separated by the separator, records by a
/// newline or CRLF; a field in double quotes may contain separators,
/// newlines and doubled double quotes, which stand for one. Upstream parses
/// bytes, and the separator is the low byte of its character; an error shows
/// its line as bytes, as Latin-1 characters.
pub fn parse_csv(sep: char, name: &str, text: &str) -> Result<Vec<CsvRecord>, String> {
    let sep = (u32::from(sep) & 0xff) as u8;
    let b = text.as_bytes();
    let field_end = |c: u8| c == sep || c == b'"' || c == b'\n' || c == b'\r';
    let mut records = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let mut record = Vec::new();
        loop {
            // a field; afterwards, what else could have continued it
            let (field, hint) = if b[i] == b'"' {
                let mut value = Vec::new();
                i += 1;
                loop {
                    match b.get(i) {
                        None => {
                            let expected = [
                                Item::tokens("\""),
                                Item::label("escaped double-quote"),
                                Item::label("unescaped character"),
                            ];
                            let e = ParseError::trivial(i, Some(Item::EndOfInput), &expected);
                            return Err(pretty_bytes(&e, name, b));
                        }
                        Some(b'"') if b.get(i + 1) == Some(&b'"') => {
                            value.push(b'"');
                            i += 2;
                        }
                        Some(b'"') => {
                            i += 1;
                            break;
                        }
                        Some(&c) => {
                            value.push(c);
                            i += 1;
                        }
                    }
                }
                (value, None)
            } else {
                let start = i;
                while i < b.len() && !field_end(b[i]) {
                    i += 1;
                }
                let hint = if i > start {
                    "unescaped character"
                } else {
                    "field"
                };
                (b[start..i].to_vec(), Some(hint))
            };
            record.push(String::from_utf8_lossy(&field).into_owned());
            match b.get(i) {
                Some(&c) if c == sep => i += 1,
                Some(b'\n') => {
                    i += 1;
                    break;
                }
                Some(b'\r') if b.get(i + 1) == Some(&b'\n') => {
                    i += 2;
                    break;
                }
                None => break,
                Some(&c) => {
                    let mut expected = BTreeSet::from([
                        Item::Tokens(char::from(sep).to_string()),
                        Item::label("end of line"),
                        Item::EndOfInput,
                    ]);
                    expected.extend(hint.map(Item::label));
                    let e = ParseError::Trivial {
                        offset: i,
                        unexpected: Some(Item::Tokens(char::from(c).to_string())),
                        expected,
                    };
                    return Err(pretty_bytes(&e, name, b));
                }
            }
        }
        records.push(record);
    }
    Ok(records)
}

/// A parse error in bytes, shown with each byte as a Latin-1 character, as
/// megaparsec shows an error in a byte string.
fn pretty_bytes(e: &ParseError, name: &str, b: &[u8]) -> String {
    let latin1: String = b.iter().map(|&c| char::from(c)).collect();
    let at = |offset: usize| {
        latin1
            .char_indices()
            .nth(offset)
            .map_or(latin1.len(), |(i, _)| i)
    };
    let e = match e.clone() {
        ParseError::Trivial {
            offset,
            unexpected,
            expected,
        } => ParseError::Trivial {
            offset: at(offset),
            unexpected,
            expected,
        },
        e => e,
    };
    e.pretty(name, &latin1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_records_and_quotes() {
        let parsed = parse_csv(',', "", "a,b\n\"c,\"\"d\"\"\",\r\n\"multi\nline\",x\n").unwrap();
        assert_eq!(
            parsed,
            vec![
                vec!["a".to_string(), "b".to_string()],
                vec!["c,\"d\"".to_string(), String::new()],
                vec!["multi\nline".to_string(), "x".to_string()],
            ]
        );
        assert_eq!(
            parse_csv(';', "", "1;2\n").unwrap(),
            vec![vec!["1".to_string(), "2".to_string()]]
        );
        assert!(parse_csv(',', "", "").unwrap().is_empty());
    }

    #[test]
    fn errors_as_megaparsec_shows_them() {
        assert_eq!(
            parse_csv(',', "f", "2022-01-01,B\"B\",C\n").unwrap_err(),
            "f:1:13:\n  |\n1 | 2022-01-01,B\"B\",C\n  |             ^\nunexpected '\"'\n\
             expecting ',', end of input, end of line, or unescaped character\n"
        );
        assert_eq!(
            parse_csv(',', "f", "a,\"B\"x\n").unwrap_err(),
            "f:1:6:\n  |\n1 | a,\"B\"x\n  |      ^\nunexpected 'x'\n\
             expecting ',', end of input, or end of line\n"
        );
        assert_eq!(
            parse_csv(',', "f", "a,\rb\n").unwrap_err(),
            "f:1:3:\n  |\n1 | a,\rb\n  |   ^\nunexpected carriage return\n\
             expecting ',', end of input, end of line, or field\n"
        );
        assert_eq!(
            parse_csv(',', "f", "a,b\n\"\n").unwrap_err(),
            "f:3:1:\n  |\n3 | <empty line>\n  | ^\nunexpected end of input\n\
             expecting '\"', escaped double-quote, or unescaped character\n"
        );
        // bytes beyond ASCII show as Latin-1 characters; C1 controls take no column
        assert_eq!(
            parse_csv(',', "f", "a,é🎅B\"\n").unwrap_err(),
            "f:1:7:\n  |\n1 | a,Ã©ð\u{9f}\u{8e}\u{85}B\"\n  |       ^\nunexpected '\"'\n\
             expecting ',', end of input, end of line, or unescaped character\n"
        );
    }
}
