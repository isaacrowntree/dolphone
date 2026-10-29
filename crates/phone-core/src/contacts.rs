//! Contacts as vCard files.
//!
//! The address book is a directory of `.vcf` files, one card or many per file, so a
//! person can copy an export from their phone straight in and edit it with anything.
//! This reads the fields a phone needs (name and numbers) from vCard 2.1, 3.0 and
//! 4.0, and writes 3.0, which every importer accepts.

use std::path::{Path, PathBuf};

use crate::number;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contact {
    pub name: String,
    pub numbers: Vec<Phone>,
    /// The file it came from, so an edit can be written back to the same place.
    pub source: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Phone {
    /// Canonical form (see [`number::canonical`]).
    pub number: String,
    /// "mobile", "home", "work" or whatever the card said, lowercased.
    pub kind: String,
}

/// Every card in `text`. Cards with no name and no number are skipped.
pub fn parse(text: &str, region: &str) -> Vec<Contact> {
    let mut out = Vec::new();
    let mut current: Option<Contact> = None;
    for line in unfold(text) {
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("BEGIN:VCARD") {
            current = Some(Contact { name: String::new(), numbers: Vec::new(), source: None });
            continue;
        }
        if upper.starts_with("END:VCARD") {
            if let Some(c) = current.take() {
                if !c.name.is_empty() || !c.numbers.is_empty() {
                    out.push(c);
                }
            }
            continue;
        }
        let Some(card) = current.as_mut() else { continue };
        let Some((head, value)) = line.split_once(':') else { continue };
        let mut parts = head.split(';');
        let prop = parts.next().unwrap_or("").to_ascii_uppercase();
        // Group prefixes ("item1.TEL") are allowed by the spec and common in exports.
        let prop = prop.rsplit('.').next().unwrap_or("").to_string();
        let params: Vec<String> = parts.map(|p| p.to_ascii_lowercase()).collect();
        match prop.as_str() {
            "FN" => card.name = unescape(value).trim().to_string(),
            "N" if card.name.is_empty() => {
                // Family;Given;Middle;Prefix;Suffix, shown as "Given Family".
                let f: Vec<&str> = value.split(';').collect();
                let given = f.get(1).copied().unwrap_or("");
                let family = f.first().copied().unwrap_or("");
                card.name = format!("{given} {family}").trim().to_string();
            }
            "TEL" => {
                let raw = value.trim_start_matches("tel:");
                let canonical = number::canonical(raw, region);
                if !canonical.is_empty() {
                    card.numbers.push(Phone { number: canonical, kind: kind_of(&params) });
                }
            }
            _ => {}
        }
    }
    out
}

/// vCard lines may be folded: a line starting with a space or tab continues the one
/// before it.
fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.lines() {
        if let Some(rest) = raw.strip_prefix([' ', '\t']) {
            if let Some(last) = lines.last_mut() {
                last.push_str(rest);
                continue;
            }
        }
        lines.push(raw.trim_end_matches('\r').to_string());
    }
    lines
}

fn unescape(v: &str) -> String {
    v.replace("\\,", ",").replace("\\;", ";").replace("\\n", " ").replace("\\\\", "\\")
}

fn kind_of(params: &[String]) -> String {
    for p in params {
        let p = p.trim_start_matches("type=");
        for t in p.split(',') {
            match t.trim_matches('"') {
                "cell" | "mobile" | "iphone" => return "mobile".into(),
                "home" => return "home".into(),
                "work" => return "work".into(),
                "main" | "pref" | "voice" => {}
                other if !other.is_empty() && !other.contains('=') => return other.into(),
                _ => {}
            }
        }
    }
    "phone".into()
}

/// A card as vCard 3.0.
pub fn write(c: &Contact) -> String {
    let mut s = String::from("BEGIN:VCARD\r\nVERSION:3.0\r\n");
    let name = c.name.replace(',', "\\,").replace(';', "\\;");
    s.push_str(&format!("FN:{name}\r\nN:;{name};;;\r\n"));
    for p in &c.numbers {
        let t = if p.kind == "mobile" { "CELL" } else { &p.kind.to_ascii_uppercase() };
        s.push_str(&format!("TEL;TYPE={t}:{}\r\n", p.number));
    }
    s.push_str("END:VCARD\r\n");
    s
}

/// An address book loaded from a directory of `.vcf` files.
#[derive(Default)]
pub struct Book {
    pub contacts: Vec<Contact>,
}

impl Book {
    pub fn load(dir: &Path, region: &str) -> Self {
        let mut contacts = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else { return Self::default() };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("vcf")))
            .collect();
        paths.sort();
        for path in paths {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            for mut c in parse(&text, region) {
                c.source = Some(path.clone());
                contacts.push(c);
            }
        }
        contacts.sort_by_key(|c| c.name.to_lowercase());
        Self { contacts }
    }

    /// The contact a number belongs to, if any.
    pub fn lookup(&self, num: &str) -> Option<&Contact> {
        if num.is_empty() {
            return None;
        }
        self.contacts.iter().find(|c| c.numbers.iter().any(|p| number::same_party(&p.number, num)))
    }

    /// The name to show for a number: the contact's, or the number formatted.
    pub fn name_for(&self, num: &str, region: &str) -> String {
        if num.is_empty() {
            return "Unknown".into();
        }
        self.lookup(num).map(|c| c.name.clone()).unwrap_or_else(|| number::display(num, region))
    }

    /// Contacts whose name or number contains `query` (digits match numbers, letters
    /// match names), for the dialer's live match.
    pub fn search(&self, query: &str) -> Vec<&Contact> {
        let q = query.to_lowercase();
        let qd = number::digits(query);
        self.contacts
            .iter()
            .filter(|c| {
                (!q.is_empty() && c.name.to_lowercase().contains(&q))
                    || (qd.len() >= 3
                        && c.numbers.iter().any(|p| {
                            number::digits(&p.number).contains(&qd)
                                || number::digits(&number::display(&p.number, number::DEFAULT_REGION)).contains(&qd)
                        }))
            })
            .collect()
    }

    /// Save a new contact as its own file in `dir`.
    pub fn add(&mut self, dir: &Path, mut c: Contact) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let stem: String =
            c.name.chars().map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '-' }).collect();
        let mut path = dir.join(format!("{}.vcf", stem.trim_matches('-')));
        let mut n = 2;
        while path.exists() {
            path = dir.join(format!("{}-{n}.vcf", stem.trim_matches('-')));
            n += 1;
        }
        std::fs::write(&path, write(&c))?;
        c.source = Some(path);
        self.contacts.push(c);
        self.contacts.sort_by_key(|c| c.name.to_lowercase());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Alex Example\r\nN:Example;Alex;;;\r\nTEL;TYPE=CELL:0491 570 156\r\nitem1.TEL;type=WORK:+61 2 9374 4000\r\nEND:VCARD\r\nBEGIN:VCARD\r\nVERSION:2.1\r\nN:Sample;Sam\r\nTEL;HOME:0491570157\r\nEND:VCARD\r\nBEGIN:VCARD\r\nVERSION:4.0\r\nFN:Folded\r\n  Name\r\nTEL;VALUE=uri;TYPE=\"voice,cell\":tel:+61491570158\r\nEND:VCARD\r\n";

    #[test]
    fn parses_three_versions() {
        let cards = parse(SAMPLE, "AU");
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[0].name, "Alex Example");
        assert_eq!(cards[0].numbers[0].number, "+61491570156");
        assert_eq!(cards[0].numbers[0].kind, "mobile");
        assert_eq!(cards[0].numbers[1].kind, "work");
        assert_eq!(cards[1].name, "Sam Sample");
        assert_eq!(cards[1].numbers[0].kind, "home");
        assert_eq!(cards[2].name, "Folded Name");
        assert_eq!(cards[2].numbers[0].kind, "mobile");
    }

    #[test]
    fn round_trips() {
        let c = &parse(SAMPLE, "AU")[0];
        let again = &parse(&write(c), "AU")[0];
        assert_eq!(again.name, c.name);
        assert_eq!(again.numbers, c.numbers);
    }

    #[test]
    fn book_lookup_and_search() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("all.vcf"), SAMPLE).unwrap();
        let book = Book::load(dir.path(), "AU");
        assert_eq!(book.name_for("+61491570157", "AU"), "Sam Sample");
        assert_eq!(book.name_for("0491570156", "AU"), "Alex Example");
        assert_eq!(book.name_for("+61491570199", "AU"), "0491 570 199");
        assert_eq!(book.search("sam").len(), 1);
        assert_eq!(book.search("0491 570").len(), 3);
        assert_eq!(book.search("57015").len(), 3);
    }

    #[test]
    fn add_writes_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut book = Book::default();
        let c = Contact {
            name: "New Person".into(),
            numbers: vec![Phone { number: "+61491570159".into(), kind: "mobile".into() }],
            source: None,
        };
        book.add(dir.path(), c.clone()).unwrap();
        book.add(dir.path(), c).unwrap();
        assert!(dir.path().join("new-person.vcf").exists());
        assert!(dir.path().join("new-person-2.vcf").exists());
        assert_eq!(Book::load(dir.path(), "AU").contacts.len(), 2);
    }
}
