//! Reading the people of a domain move from a CSV file (docs/moving.md, "A list from a file"): what
//! spreadsheets export, with `;` or `,` (or tabs) between the cells, quotes around cells that need
//! them, and a header line or none.
//!
//! With a header, its names say which column is which, in German or English. Without one the
//! columns are, in this order: old address, password, name, new address, quota, aliases, login.
//! Every problem names the line it is on, so the admin can fix the file.

use serde::Serialize;

/// The longest text taken: two thousand people fit many times over.
pub const MAX_CSV_BYTES: usize = 1024 * 1024;
/// Aliases one line may bring.
const MAX_ALIASES: usize = 20;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CsvRow {
    /// The line in the text this row starts on, from 1.
    pub line: usize,
    pub old_address: String,
    /// Empty: the old address is the login.
    pub login: String,
    pub password: String,
    pub name: String,
    /// The address here; the old address's local part on the move's domain when the file has none.
    pub target: String,
    pub quota_bytes: Option<i64>,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CsvProblem {
    pub line: usize,
    /// The column it is about: oldAddress, password, target, quota, aliases or line.
    pub field: &'static str,
    /// addressInvalid, passwordMissing, targetInvalid, quotaInvalid, aliasInvalid, duplicate,
    /// tooManyRows, tooLarge, quoteOpen.
    pub code: &'static str,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CsvRead {
    pub rows: Vec<CsvRow>,
    pub problems: Vec<CsvProblem>,
    /// The delimiter found: `;`, `,` or a tab.
    pub delimiter: String,
    pub header: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    OldAddress,
    Login,
    Password,
    Name,
    Target,
    Quota,
    Aliases,
    Ignored,
}

const POSITIONS: [Column; 7] =
    [Column::OldAddress, Column::Password, Column::Name, Column::Target, Column::Quota, Column::Aliases, Column::Login];

fn column_of(header: &str) -> Option<Column> {
    let name: String = header
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    Some(match name.as_str() {
        "alte adresse" | "old address" | "email" | "e mail" | "email address" | "e mail adresse" | "address"
        | "adresse" | "quelle" | "source" | "from" | "von" | "mail" => Column::OldAddress,
        "login" | "benutzer" | "benutzername" | "user" | "username" | "user name" | "old login" | "alter login"
        | "anmeldename" => Column::Login,
        "passwort" | "password" | "kennwort" | "pass" | "pw" | "altes passwort" | "old password" => Column::Password,
        "name" | "anzeigename" | "display name" | "displayname" | "vollständiger name" | "full name" => Column::Name,
        "neue adresse" | "new address" | "ziel" | "target" | "zieladresse" | "target address" | "postfach"
        | "mailbox" | "to" | "nach" => Column::Target,
        "quota" | "kontingent" | "speicher" | "quota mb" | "speicherplatz" | "quota in mb" => Column::Quota,
        "aliase" | "aliases" | "alias" | "weitere adressen" | "aliasse" => Column::Aliases,
        _ => return None,
    })
}

/// Cuts the text into records of cells. Quotes may hold the delimiter, line breaks and doubled
/// quotes. Each record comes with the line it starts on.
fn records(text: &str, delimiter: char) -> (Vec<(usize, Vec<String>)>, bool) {
    let mut records = Vec::new();
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut line = 1;
    let mut start = 1;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                cell.push('"');
            }
            '"' if quoted => quoted = false,
            '"' if cell.trim().is_empty() => {
                cell.clear();
                quoted = true;
            }
            c if c == delimiter && !quoted => cells.push(std::mem::take(&mut cell)),
            '\r' if !quoted => {}
            '\n' if !quoted => {
                cells.push(std::mem::take(&mut cell));
                records.push((start, std::mem::take(&mut cells)));
                line += 1;
                start = line;
            }
            '\n' => {
                line += 1;
                cell.push('\n');
            }
            c => cell.push(c),
        }
    }
    let open = quoted;
    if !cell.is_empty() || !cells.is_empty() {
        cells.push(cell);
        records.push((start, cells));
    }
    let records = records
        .into_iter()
        .filter(|(_, cells)| {
            let first = cells.iter().find(|cell| !cell.trim().is_empty());
            first.is_some_and(|first| !first.trim_start().starts_with('#'))
        })
        .collect();
    (records, open)
}

/// The delimiter of the first line with content: the one it has most of outside quotes.
fn delimiter_of(text: &str) -> char {
    let first = text.lines().find(|line| !line.trim().is_empty()).unwrap_or_default();
    let mut counts = [(';', 0usize), (',', 0), ('\t', 0)];
    let mut quoted = false;
    for c in first.chars() {
        if c == '"' {
            quoted = !quoted;
        } else if !quoted && let Some(entry) = counts.iter_mut().find(|(d, _)| *d == c) {
            entry.1 += 1;
        }
    }
    // Ties go to the semicolon: German spreadsheets write commas in numbers.
    counts.iter().fold((';', 0), |best, &(d, n)| if n > best.1 { (d, n) } else { best }).0
}

/// A quota as people write it: a number of megabytes, or with KB, MB, GB, TB (or K, M, G, T).
pub fn parse_quota(text: &str) -> Option<i64> {
    let text = text.trim().to_ascii_lowercase().replace(' ', "");
    if text.is_empty() {
        return None;
    }
    let split = text.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number.replace(',', ".").parse().ok()?;
    let factor: f64 = match unit.trim_end_matches('b').trim_end_matches("i") {
        "" | "m" => 1024.0 * 1024.0,
        "k" => 1024.0,
        "g" => 1024.0 * 1024.0 * 1024.0,
        "t" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    let bytes = number * factor;
    (bytes.is_finite() && (0.0..9.0e18).contains(&bytes)).then_some(bytes.round() as i64)
}

fn address_ok(address: &str) -> bool {
    uwumail_store::normalize_address(address).is_ok()
}

/// Reads a CSV text into rows. `domain` is the move's domain, for the new addresses the file
/// leaves out.
pub fn read(text: &str, domain: Option<&str>, max_rows: usize) -> CsvRead {
    let mut read = CsvRead::default();
    if text.len() > MAX_CSV_BYTES {
        read.problems.push(CsvProblem { line: 1, field: "line", code: "tooLarge" });
        return read;
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let delimiter = delimiter_of(text);
    read.delimiter = delimiter.to_string();
    let (records, open) = records(text, delimiter);
    let mut records = records.into_iter().peekable();
    let mut columns: Vec<Column> = POSITIONS.to_vec();
    if let Some((_, first)) = records.peek() {
        let named: Vec<Option<Column>> = first.iter().map(|cell| column_of(cell)).collect();
        // A header names the old address column at least; a first line with an address in it is data.
        if named.contains(&Some(Column::OldAddress)) && !first.iter().any(|cell| cell.contains('@')) {
            columns = named.into_iter().map(|column| column.unwrap_or(Column::Ignored)).collect();
            read.header = true;
            records.next();
        }
    }
    let mut seen_old = std::collections::HashSet::new();
    let mut seen_target = std::collections::HashSet::new();
    for (line, cells) in records {
        if read.rows.len() >= max_rows {
            read.problems.push(CsvProblem { line, field: "line", code: "tooManyRows" });
            break;
        }
        let mut row = CsvRow { line, ..CsvRow::default() };
        let mut quota = None;
        for (index, cell) in cells.iter().enumerate() {
            let value = cell.trim();
            match columns.get(index).copied().unwrap_or(Column::Ignored) {
                Column::OldAddress => row.old_address = value.to_lowercase(),
                Column::Login => row.login = value.to_owned(),
                // Passwords are taken as they are: spaces at the ends may be part of them.
                Column::Password => row.password = cell.trim_matches(['\r', '\n']).to_owned(),
                Column::Name => row.name = value.to_owned(),
                Column::Target => row.target = value.to_lowercase(),
                Column::Quota => quota = Some(value.to_owned()),
                Column::Aliases => {
                    row.aliases = value
                        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '|'))
                        .filter(|alias| !alias.is_empty())
                        .map(str::to_lowercase)
                        .collect();
                }
                Column::Ignored => {}
            }
        }
        if !address_ok(&row.old_address) {
            read.problems.push(CsvProblem { line, field: "oldAddress", code: "addressInvalid" });
        } else if !seen_old.insert(row.old_address.clone()) {
            read.problems.push(CsvProblem { line, field: "oldAddress", code: "duplicate" });
        }
        if row.password.is_empty() {
            read.problems.push(CsvProblem { line, field: "password", code: "passwordMissing" });
        }
        if row.target.is_empty()
            && let (Some(domain), Some((local, _))) = (domain, row.old_address.rsplit_once('@'))
        {
            row.target = format!("{local}@{}", domain.trim().to_lowercase());
        }
        if !row.target.is_empty() {
            let on_domain =
                domain.is_none_or(|domain| row.target.ends_with(&format!("@{}", domain.trim().to_lowercase())));
            if !address_ok(&row.target) || !on_domain {
                read.problems.push(CsvProblem { line, field: "target", code: "targetInvalid" });
            } else if !seen_target.insert(row.target.clone()) {
                read.problems.push(CsvProblem { line, field: "target", code: "duplicate" });
            }
        }
        if let Some(quota) = quota.filter(|quota| !quota.is_empty()) {
            match parse_quota(&quota) {
                Some(bytes) => row.quota_bytes = Some(bytes),
                None => read.problems.push(CsvProblem { line, field: "quota", code: "quotaInvalid" }),
            }
        }
        if row.aliases.len() > MAX_ALIASES || row.aliases.iter().any(|alias| !address_ok(alias)) {
            read.problems.push(CsvProblem { line, field: "aliases", code: "aliasInvalid" });
            row.aliases.truncate(MAX_ALIASES);
        }
        read.rows.push(row);
    }
    if open {
        let line = text.lines().count().max(1);
        read.problems.push(CsvProblem { line, field: "line", code: "quoteOpen" });
    }
    read
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(read: &CsvRead) -> Vec<(usize, &'static str, &'static str)> {
        read.problems.iter().map(|problem| (problem.line, problem.field, problem.code)).collect()
    }

    #[test]
    fn a_german_export_with_header_and_semicolons() {
        let text = "\u{feff}Alte Adresse;Passwort;Anzeigename;Neue Adresse;Quota;Aliase\r\n\
                    mini@example.org;geheim;Mini Muster;;5 GB;info@example.org kontakt@example.org\r\n\
                    nyu@example.org;\"mit;semikolon \"\"und\"\" Quote\";\"Nyu, die Katze\";katze@example.org;500;\r\n";
        let read = read(text, Some("example.org"), 100);
        assert!(read.header && read.delimiter == ";", "{read:?}");
        assert_eq!(codes(&read), vec![]);
        let mini = &read.rows[0];
        assert_eq!((mini.line, mini.target.as_str(), mini.name.as_str()), (2, "mini@example.org", "Mini Muster"));
        assert_eq!(mini.quota_bytes, Some(5 * 1024 * 1024 * 1024));
        assert_eq!(mini.aliases, vec!["info@example.org", "kontakt@example.org"]);
        let nyu = &read.rows[1];
        assert_eq!(nyu.password, "mit;semikolon \"und\" Quote");
        assert_eq!((nyu.name.as_str(), nyu.target.as_str()), ("Nyu, die Katze", "katze@example.org"));
        assert_eq!(nyu.quota_bytes, Some(500 * 1024 * 1024));
    }

    #[test]
    fn commas_without_header_go_by_position() {
        let text = "mini@example.org,geheim,Mini\n\n# a comment\nleni@example.org,pw2,Leni,,1g,,leni.alt\n";
        let read = read(text, Some("example.org"), 100);
        assert!(!read.header);
        assert_eq!(read.delimiter, ",");
        assert_eq!(read.rows.len(), 2);
        assert_eq!(read.rows[1].line, 4);
        assert_eq!(read.rows[1].login, "leni.alt");
        assert_eq!(read.rows[1].quota_bytes, Some(1024 * 1024 * 1024));
        assert_eq!(read.rows[0].login, "", "the old address is the login");
    }

    #[test]
    fn english_headers_in_any_order_and_tabs() {
        let text = "Password\tE-Mail\tUsername\tIgnored\nsecret\tmini@example.org\tmini\tx\n";
        let read = read(text, None, 100);
        assert!(read.header);
        assert_eq!(read.delimiter, "\t");
        let mini = &read.rows[0];
        assert_eq!(
            (mini.old_address.as_str(), mini.login.as_str(), mini.password.as_str()),
            ("mini@example.org", "mini", "secret")
        );
        assert_eq!(mini.target, "", "without a domain nothing is made up");
    }

    #[test]
    fn problems_name_their_line() {
        let text = "Adresse;Passwort;Ziel;Quota;Aliase\nkein-at;pw;;;\nmini@example.org;;;viel;\n\
                    mini@example.org;pw;andere@example.net;;nicht-gültig\n\"offen@example.org;pw\n";
        let read = read(text, Some("example.org"), 100);
        assert_eq!(
            codes(&read),
            vec![
                (2, "oldAddress", "addressInvalid"),
                (3, "password", "passwordMissing"),
                (3, "quota", "quotaInvalid"),
                (4, "oldAddress", "duplicate"),
                (4, "target", "targetInvalid"),
                (4, "aliases", "aliasInvalid"),
                (5, "oldAddress", "addressInvalid"),
                (5, "password", "passwordMissing"),
                (5, "line", "quoteOpen"),
            ]
        );
    }

    #[test]
    fn limits_hold() {
        let many: String = (0..5).map(|n| format!("p{n}@example.org;pw\n")).collect();
        let read = read(&many, None, 3);
        assert_eq!(read.rows.len(), 3);
        assert_eq!(codes(&read), vec![(4, "line", "tooManyRows")]);
        let huge = "x".repeat(MAX_CSV_BYTES + 1);
        assert_eq!(codes(&super::read(&huge, None, 3)), vec![(1, "line", "tooLarge")]);
        // Hostile text does not panic: lone quotes, multibyte characters, empty cells.
        let odd = "\"\"\"ä\";\u{1F600}\n;;;\n\"";
        let _ = super::read(odd, Some("example.org"), 10);
    }

    #[test]
    fn quotas_as_people_write_them() {
        assert_eq!(parse_quota("500"), Some(500 * 1024 * 1024));
        assert_eq!(parse_quota("1,5 GB"), Some(1536 * 1024 * 1024));
        assert_eq!(parse_quota("2GiB"), Some(2 * 1024 * 1024 * 1024));
        assert_eq!(parse_quota("750 MB"), Some(750 * 1024 * 1024));
        assert_eq!(parse_quota("0"), Some(0));
        assert_eq!(parse_quota("viel"), None);
        assert_eq!(parse_quota("-5"), None);
        assert_eq!(parse_quota("1e30"), None);
    }
}
