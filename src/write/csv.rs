//! Writing tables as CSV and TSV (Hledger.Write.Csv).

/// Records as CSV: every field in double quotes, a double quote in a field
/// doubled, a record per line (printCSV).
pub fn print_csv<S: AsRef<str>>(
    records: impl IntoIterator<Item = impl IntoIterator<Item = S>>,
) -> String {
    let mut out = String::new();
    for r in records {
        for (i, f) in r.into_iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('"');
            for (j, part) in f.as_ref().split('"').enumerate() {
                if j > 0 {
                    out.push_str("\"\"");
                }
                out.push_str(part);
            }
            out.push('"');
        }
        out.push('\n');
    }
    out
}

/// Records as CSV for the csv format, otherwise as TSV.
pub fn print_delimited<S: AsRef<str>>(
    fmt: &str,
    records: impl IntoIterator<Item = impl IntoIterator<Item = S>>,
) -> String {
    if fmt == "csv" {
        print_csv(records)
    } else {
        print_tsv(records)
    }
}

/// Records as TSV: fields separated by tabs, a tab, newline or carriage
/// return in a field replaced by a space, a record per line (printTSV).
pub fn print_tsv<S: AsRef<str>>(
    records: impl IntoIterator<Item = impl IntoIterator<Item = S>>,
) -> String {
    let mut out = String::new();
    for r in records {
        for (i, f) in r.into_iter().enumerate() {
            if i > 0 {
                out.push('\t');
            }
            out.extend((f.as_ref().chars()).map(|c| match c {
                '\t' | '\n' | '\r' => ' ',
                c => c,
            }));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_quoting() {
        let records = [vec!["a", "say \"hi\""], vec!["", "x,y\nz"]];
        assert_eq!(
            print_csv(&records),
            "\"a\",\"say \"\"hi\"\"\"\n\"\",\"x,y\nz\"\n"
        );
    }

    #[test]
    fn tsv_whitespace() {
        let records = [vec!["a\tb", "c\r\nd"], vec!["e"]];
        assert_eq!(print_tsv(&records), "a b\tc  d\ne\n");
    }
}
