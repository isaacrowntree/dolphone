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
    /// Its place among the cards in that file (a file may hold several).
    pub card: usize,
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
            current = Some(Contact { name: String::new(), numbers: Vec::new(), source: None, card: 0 });
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
            for (i, mut c) in parse(&text, region).into_iter().enumerate() {
                c.source = Some(path.clone());
                c.card = i;
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
        if num.contains(',') {
            // A group text's thread: every member.
            return num.split(',').map(|n| self.name_for(n, region)).collect::<Vec<_>>().join(", ");
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

    /// Contacts whose name matches keypad digits the phone way: 2 is ABC, 3 is DEF,
    /// and so on, against the start of the name or of any word in it ("567" and "362"
    /// both find "Jordan Doe"). For dialing by name on a keypad.
    pub fn search_t9(&self, digits: &str) -> Vec<&Contact> {
        let d: String = digits.chars().filter(|c| c.is_ascii_digit()).collect();
        if d.len() < 2 || d.chars().any(|c| c == '0' || c == '1') {
            return Vec::new();
        }
        self.contacts
            .iter()
            .filter(|c| {
                let words: Vec<String> = c.name.split_whitespace().map(t9_word).collect();
                let whole: String = words.concat();
                whole.starts_with(&d) || words.iter().any(|w| w.starts_with(&d))
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
        c.card = 0;
        self.contacts.push(c);
        self.contacts.sort_by_key(|c| c.name.to_lowercase());
        Ok(())
    }
}

/// What an import did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Imported {
    pub added: u32,
    /// Matched a contact already here (same name, or a number already known) and
    /// gave it any numbers it lacked.
    pub merged: u32,
    pub unchanged: u32,
}

/// An import never takes more than this many cards: a phonebook is hundreds, and a
/// file of millions is a mistake or an attack on the disk.
pub const IMPORT_MAX: usize = 5000;

impl Contact {
    /// The contact's id: its file's name without .vcf, and its place in the file
    /// after a # when it is not the first card there.
    pub fn id(&self) -> String {
        let stem = self
            .source
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if self.card == 0 {
            stem
        } else {
            format!("{stem}#{}", self.card)
        }
    }
}

impl Book {
    pub fn get(&self, id: &str) -> Option<&Contact> {
        self.contacts.iter().find(|c| c.id() == id)
    }

    /// Bring in cards from another phone or an export. Only names and numbers are
    /// kept (parse drops the rest). A card that matches one already here, by name
    /// or by any number, adds its missing numbers to it instead of making a twin,
    /// so importing the same phonebook twice changes nothing.
    pub fn import(&mut self, dir: &Path, cards: Vec<Contact>) -> std::io::Result<Imported> {
        let mut out = Imported::default();
        for mut card in cards.into_iter().take(IMPORT_MAX) {
            card.name = card.name.trim().to_string();
            card.numbers.retain(|p| !number::digits(&p.number).is_empty());
            if card.name.is_empty() && card.numbers.is_empty() {
                continue;
            }
            if card.name.is_empty() {
                card.name = card.numbers[0].number.clone();
            }
            let same = self.contacts.iter().position(|c| {
                c.name.eq_ignore_ascii_case(&card.name)
                    || c.numbers.iter().any(|a| card.numbers.iter().any(|b| number::same_party(&a.number, &b.number)))
            });
            match same {
                Some(i) => {
                    let c = &mut self.contacts[i];
                    let new: Vec<Phone> = card
                        .numbers
                        .into_iter()
                        .filter(|b| !c.numbers.iter().any(|a| number::same_party(&a.number, &b.number)))
                        .collect();
                    if new.is_empty() {
                        out.unchanged += 1;
                        continue;
                    }
                    c.numbers.extend(new);
                    if let Some(path) = c.source.clone() {
                        self.rewrite(&path)?;
                    }
                    out.merged += 1;
                }
                None => {
                    self.add(dir, card)?;
                    out.added += 1;
                }
            }
        }
        Ok(out)
    }

    /// Replace a contact's name and numbers, in its own file.
    pub fn update(&mut self, id: &str, name: &str, numbers: Vec<Phone>) -> std::io::Result<()> {
        let c = self
            .contacts
            .iter_mut()
            .find(|c| c.id() == id)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such contact"))?;
        c.name = name.trim().to_string();
        c.numbers = numbers;
        if let Some(path) = c.source.clone() {
            self.rewrite(&path)?;
        }
        self.contacts.sort_by_key(|c| c.name.to_lowercase());
        Ok(())
    }

    /// Write back every card that lives in `path`, in its order, so changing one
    /// card in a file of many keeps the others. A file left with none is removed.
    fn rewrite(&mut self, path: &Path) -> std::io::Result<()> {
        let mut here: Vec<&mut Contact> =
            self.contacts.iter_mut().filter(|c| c.source.as_deref() == Some(path)).collect();
        here.sort_by_key(|c| c.card);
        if here.is_empty() {
            return match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        let mut text = String::new();
        for (i, c) in here.into_iter().enumerate() {
            c.card = i;
            text.push_str(&write(c));
        }
        std::fs::write(path, text)
    }

    /// Delete a contact and its file.
    pub fn remove(&mut self, id: &str) -> std::io::Result<()> {
        let i = self
            .contacts
            .iter()
            .position(|c| c.id() == id)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such contact"))?;
        let gone = self.contacts.remove(i);
        if let Some(path) = gone.source {
            self.rewrite(&path)?;
        }
        Ok(())
    }
}

/// A word as keypad digits. Letters outside A-Z (after removing accents roughly)
/// map to nothing, so they never match.
fn t9_word(w: &str) -> String {
    w.chars()
        .filter_map(|ch| {
            let c = match ch.to_ascii_lowercase() {
                'à' | 'á' | 'â' | 'ä' | 'ã' => 'a',
                'é' | 'è' | 'ê' | 'ë' => 'e',
                'í' | 'ï' | 'î' => 'i',
                'ó' | 'ö' | 'ô' | 'õ' => 'o',
                'ú' | 'ü' | 'û' => 'u',
                'ç' => 'c',
                'ñ' => 'n',
                other => other,
            };
            Some(match c {
                'a'..='c' => '2',
                'd'..='f' => '3',
                'g'..='i' => '4',
                'j'..='l' => '5',
                'm'..='o' => '6',
                'p'..='s' => '7',
                't'..='v' => '8',
                'w'..='z' => '9',
                _ => return None,
            })
        })
        .collect()
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
    fn t9_finds_names() {
        let book = Book { contacts: parse(SAMPLE, "AU") };
        let names = |d: &str| book.search_t9(d).iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        assert_eq!(names("2539"), vec!["Alex Example"]);
        assert_eq!(names("3926"), vec!["Alex Example"], "matches the second word");
        assert_eq!(names("726"), vec!["Sam Sample"]);
        assert!(names("1").is_empty());
        assert!(names("0491").is_empty(), "a number is not a name");
    }

    #[test]
    fn add_writes_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut book = Book::default();
        let c = Contact {
            name: "New Person".into(),
            numbers: vec![Phone { number: "+61491570159".into(), kind: "mobile".into() }],
            source: None,
            card: 0,
        };
        book.add(dir.path(), c.clone()).unwrap();
        book.add(dir.path(), c).unwrap();
        assert!(dir.path().join("new-person.vcf").exists());
        assert!(dir.path().join("new-person-2.vcf").exists());
        assert_eq!(Book::load(dir.path(), "AU").contacts.len(), 2);
    }

    #[test]
    fn import_merges_instead_of_doubling() {
        let dir = std::env::temp_dir().join(format!("dolphone-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut b = Book::default();
        let first = parse(SAMPLE, "AU");
        let r = b.import(&dir, first.clone()).unwrap();
        assert_eq!((r.added, r.merged, r.unchanged), (3, 0, 0));
        let again = b.import(&dir, first).unwrap();
        assert_eq!((again.added, again.merged, again.unchanged), (0, 0, 3), "the same phonebook twice changes nothing");
        let extra =
            parse("BEGIN:VCARD\nFN:Alex Example\nTEL:0491570199\nEMAIL:alex@example.com\nPHOTO:xyz\nEND:VCARD\n", "AU");
        let r = b.import(&dir, extra).unwrap();
        assert_eq!(r.merged, 1);
        let alex = b.contacts.iter().find(|c| c.name == "Alex Example").unwrap();
        assert_eq!(alex.numbers.len(), 3, "the new number joins the old ones");
        let on_disk = std::fs::read_to_string(alex.source.as_ref().unwrap()).unwrap();
        assert!(!on_disk.contains("EMAIL") && !on_disk.contains("PHOTO"), "only names and numbers are kept");
        let id = alex.id();
        b.update(&id, "Alex E.", vec![Phone { number: "+61491570156".into(), kind: "cell".into() }]).unwrap();
        assert_eq!(Book::load(&dir, "AU").get(&id).unwrap().name, "Alex E.");
        b.remove(&id).unwrap();
        assert!(Book::load(&dir, "AU").get(&id).is_none());

        // A file of several cards: changing one keeps the rest.
        let many = dir.join("export.vcf");
        std::fs::write(&many, SAMPLE).unwrap();
        let mut b = Book::load(&dir, "AU");
        let folded = b.contacts.iter().find(|c| c.name == "Folded Name").unwrap().id();
        assert_eq!(folded, "export#2");
        b.remove("export#1").unwrap();
        let after = Book::load(&dir, "AU");
        assert!(after.contacts.iter().any(|c| c.name == "Folded Name"), "the others in the file stay");
        assert!(!after.contacts.iter().any(|c| c.name == "Sam Sample" && c.source.as_deref() == Some(&*many)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
