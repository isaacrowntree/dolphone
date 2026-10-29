//! Contacts as vCard files.
//!
//! The address book is a directory of `.vcf` files, one card or many per file, so a
//! person can copy an export from their phone straight in and edit it with anything.
//! This reads the fields a phone needs (name and numbers) from vCard 2.1, 3.0 and
//! 4.0, and writes 3.0, which every importer accepts.
//!
//! An edit changes only what it is about: the card's name lines (FN, N) when the
//! name changed, and its TEL lines when the numbers did. Everything else in the
//! file (emails, addresses, photos, notes, cards with no name or number, other
//! cards) is left as it was, so the files stay the person's own. Files are written
//! whole to a temporary file and renamed into place, 0600 in a 0700 directory.

use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

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
            "FN" => card.name = clean_name(&unescape(value)),
            "N" if card.name.is_empty() => {
                // Family;Given;Middle;Prefix;Suffix, shown as "Given Family".
                let f: Vec<&str> = value.split(';').collect();
                let given = f.get(1).copied().unwrap_or("");
                let family = f.first().copied().unwrap_or("");
                card.name = clean_name(&unescape(&format!("{given} {family}")));
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

/// The longest name kept, in characters.
pub const NAME_MAX: usize = 100;

/// A name as shown: what a caller shows as, so no controls, bidi overrides or
/// hidden characters (see [`crate::text::clean`]), one line, and not too long.
fn clean_name(name: &str) -> String {
    let one_line: String = name.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    crate::text::clean(one_line.trim(), NAME_MAX).trim().to_string()
}

/// A property's name, without any group prefix ("item1.TEL" is TEL).
fn prop_of(line: &str) -> String {
    let head = line.split_once(':').map(|(h, _)| h).unwrap_or(line);
    let prop = head.split(';').next().unwrap_or("").to_ascii_uppercase();
    prop.rsplit('.').next().unwrap_or("").to_string()
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

/// A name as a vCard value: escaped, so a comma, semicolon or newline in it stays
/// part of the name and cannot start another property.
fn escape(v: &str) -> String {
    v.replace('\\', "\\\\").replace(',', "\\,").replace(';', "\\;").replace('\n', "\\n").replace('\r', "")
}

fn name_lines(name: &str) -> [String; 2] {
    let name = escape(name);
    [format!("FN:{name}"), format!("N:;{name};;;")]
}

fn tel_line(p: &Phone) -> String {
    let t = if p.kind == "mobile" { "CELL".to_string() } else { p.kind.to_ascii_uppercase() };
    // A kind came from a card's parameters; keep it to what a parameter may hold.
    let t: String = t.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    let t = if t.is_empty() { "VOICE".to_string() } else { t };
    format!("TEL;TYPE={t}:{}", p.number)
}

/// A card as vCard 3.0.
pub fn write(c: &Contact) -> String {
    let mut s = String::from("BEGIN:VCARD\r\nVERSION:3.0\r\n");
    for l in name_lines(&c.name) {
        s.push_str(&l);
        s.push_str("\r\n");
    }
    for p in &c.numbers {
        s.push_str(&tel_line(p));
        s.push_str("\r\n");
    }
    s.push_str("END:VCARD\r\n");
    s
}

/// A file's text in pieces: each card's lines as they are (folded as they were),
/// and whatever lies between cards.
#[derive(Debug)]
enum Segment {
    Other(String),
    Card(Vec<String>),
}

fn segments(text: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut card: Option<Vec<String>> = None;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r').to_string();
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("BEGIN:VCARD") {
            // A card begun inside another ends that one, as parse has it.
            if let Some(partial) = card.take() {
                out.extend(partial.into_iter().map(Segment::Other));
            }
            card = Some(vec![line]);
            continue;
        }
        match card.as_mut() {
            Some(c) => {
                c.push(line);
                if upper.starts_with("END:VCARD") {
                    out.push(Segment::Card(card.take().unwrap_or_default()));
                }
            }
            None => out.push(Segment::Other(line)),
        }
    }
    if let Some(partial) = card {
        out.extend(partial.into_iter().map(Segment::Other));
    }
    out
}

/// A card's logical lines (unfolded), each with the physical lines it came from.
fn logical(lines: &[String]) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for l in lines {
        if let Some(rest) = l.strip_prefix([' ', '\t']) {
            if let Some((joined, physical)) = out.last_mut() {
                joined.push_str(rest);
                physical.push(l.clone());
                continue;
            }
        }
        out.push((l.clone(), vec![l.clone()]));
    }
    out
}

/// A TEL line's number and kind, as parse reads them; None for one parse skips.
fn tel_of(line: &str, region: &str) -> Option<Phone> {
    let (head, value) = line.split_once(':')?;
    let params: Vec<String> = head.split(';').skip(1).map(|p| p.to_ascii_lowercase()).collect();
    let canonical = number::canonical(value.trim_start_matches("tel:"), region);
    (!canonical.is_empty()).then(|| Phone { number: canonical, kind: kind_of(&params) })
}

/// A card's lines, changed from `old` to `new`: its name lines replaced if the
/// name changed, and TEL lines kept where `new` still has that number and kind,
/// dropped where it does not, and added for numbers it gains. Every other line
/// stays as it was.
fn patch_card(lines: &[String], old: &Contact, new: &Contact, region: &str) -> Vec<String> {
    let rename = new.name != old.name;
    let mut want = new.numbers.clone();
    let mut out: Vec<String> = Vec::new();
    let mut end: Vec<String> = Vec::new();
    let mut named = false;
    let mut tel_at: Option<usize> = None;
    for (joined, physical) in logical(lines) {
        match prop_of(&joined).as_str() {
            "END" => end.extend(physical),
            "FN" | "N" if rename => {
                if !named {
                    out.extend(name_lines(&new.name));
                    named = true;
                }
            }
            "TEL" => {
                match tel_of(&joined, region) {
                    Some(p) => {
                        if let Some(i) = want.iter().position(|w| *w == p) {
                            want.remove(i);
                            out.extend(physical);
                        }
                    }
                    // A line parse does not read as a number is not ours to drop.
                    None => out.extend(physical),
                }
                tel_at = Some(out.len());
            }
            _ => out.extend(physical),
        }
    }
    let at = tel_at.unwrap_or(out.len());
    let added: Vec<String> = want.iter().map(tel_line).collect();
    out.splice(at..at, added);
    if rename && !named {
        out.extend(name_lines(&new.name));
    }
    out.extend(end);
    out
}

/// Each file written gets its own temporary name.
static TEMP: AtomicU32 = AtomicU32::new(0);

/// Make the contacts directory, 0700.
fn make_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Write `data` to a new temporary file (0600) beside `path`, flushed to disk.
fn temp_beside(path: &Path, data: &[u8]) -> std::io::Result<PathBuf> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.{}.tmp", std::process::id(), TEMP.fetch_add(1, Ordering::Relaxed)));
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    let written = f.write_all(data).and_then(|_| f.sync_all());
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(tmp)
}

fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

/// Replace `path` with `data` all at once: a crash leaves the old file or the new
/// one, never half of either.
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = temp_beside(path, data)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    sync_dir(path.parent().unwrap_or(Path::new(".")));
    Ok(())
}

/// Write a new file at `path`, or say it already exists: the file appears whole,
/// and never over another.
fn write_new(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = temp_beside(path, data)?;
    let linked = std::fs::hard_link(&tmp, path);
    let _ = std::fs::remove_file(&tmp);
    linked?;
    sync_dir(path.parent().unwrap_or(Path::new(".")));
    Ok(())
}

/// Change the `card`th card in `path` (counted as [`Book::load`] counts them) from
/// `old` to `new`, or remove it for None, after checking it still is `old`: the
/// file may have changed since it was read. A file left with no cards is removed.
fn patch_file(path: &Path, card: usize, old: &Contact, new: Option<&Contact>, region: &str) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    let text = std::fs::read_to_string(path)?;
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut segs = segments(&text);
    let mut k = 0;
    let mut found = None;
    for (i, seg) in segs.iter().enumerate() {
        let Segment::Card(lines) = seg else { continue };
        if let Some(c) = parse(&lines.join("\n"), region).into_iter().next() {
            if k == card {
                found = Some((i, c));
                break;
            }
            k += 1;
        }
    }
    let (i, on_disk) = found.ok_or_else(|| Error::new(ErrorKind::NotFound, "the contact is no longer in its file"))?;
    if on_disk.name != old.name || on_disk.numbers != old.numbers {
        return Err(Error::other("the contact changed in its file since it was read; try again"));
    }
    match new {
        Some(n) => {
            if let Segment::Card(lines) = &segs[i] {
                segs[i] = Segment::Card(patch_card(lines, old, n, region));
            }
        }
        None => {
            segs.remove(i);
        }
    }
    let empty = segs.iter().all(|s| matches!(s, Segment::Other(l) if l.trim().is_empty()));
    if empty {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let mut out = String::new();
    for seg in &segs {
        let lines: &[String] = match seg {
            Segment::Other(l) => std::slice::from_ref(l),
            Segment::Card(c) => c,
        };
        for l in lines {
            out.push_str(l);
            out.push_str(nl);
        }
    }
    write_atomic(path, out.as_bytes())
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
        make_dir(dir)?;
        let text = write(&c);
        let mut n = 1;
        let path = loop {
            let path = dir.join(file_name(&c.name, n));
            match write_new(&path, text.as_bytes()) {
                Ok(()) => break path,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 10_000 => n += 1,
                Err(e) => return Err(e),
            }
        };
        c.source = Some(path);
        c.card = 0;
        self.contacts.push(c);
        self.contacts.sort_by_key(|c| c.name.to_lowercase());
        Ok(())
    }
}

/// The file name for a new contact: its name in lowercase ASCII letters and
/// digits, a dash for anything else, and "-n" after the first. A name with none of
/// those (one in another script, or all emoji) is "contact-n", as ".vcf" alone
/// would be a hidden file with no name, which load does not read.
fn file_name(name: &str, n: u32) -> String {
    let stem: String =
        name.chars().map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '-' }).collect();
    let stem = stem.trim_matches('-');
    match (stem.is_empty(), n) {
        (true, n) => format!("contact-{n}.vcf"),
        (false, 1) => format!("{stem}.vcf"),
        (false, n) => format!("{stem}-{n}.vcf"),
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
    pub fn import(&mut self, dir: &Path, cards: Vec<Contact>, region: &str) -> std::io::Result<Imported> {
        let mut out = Imported::default();
        for mut card in cards.into_iter().take(IMPORT_MAX) {
            card.name = clean_name(&card.name);
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
                    let old = &self.contacts[i];
                    let new: Vec<Phone> = card
                        .numbers
                        .into_iter()
                        .filter(|b| !old.numbers.iter().any(|a| number::same_party(&a.number, &b.number)))
                        .collect();
                    if new.is_empty() {
                        out.unchanged += 1;
                        continue;
                    }
                    let mut numbers = old.numbers.clone();
                    numbers.extend(new);
                    self.change(i, old.name.clone(), numbers, region)?;
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

    /// Replace a contact's name and numbers, in its own file. `region` is the one
    /// the book was loaded with.
    pub fn update(&mut self, id: &str, name: &str, numbers: Vec<Phone>, region: &str) -> std::io::Result<()> {
        let i = self.position(id)?;
        self.change(i, clean_name(name), numbers, region)?;
        self.contacts.sort_by_key(|c| c.name.to_lowercase());
        Ok(())
    }

    fn position(&self, id: &str) -> std::io::Result<usize> {
        self.contacts
            .iter()
            .position(|c| c.id() == id)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such contact"))
    }

    /// Change the contact at `i`, in its file (only its name and number lines),
    /// then here.
    fn change(&mut self, i: usize, name: String, numbers: Vec<Phone>, region: &str) -> std::io::Result<()> {
        let old = self.contacts[i].clone();
        // Numbers as they will read back from the file ("cell" is read as
        // "mobile"), so what is held here is what the file says.
        let numbers = numbers.into_iter().map(|p| tel_of(&tel_line(&p), region).unwrap_or(p)).collect();
        let new = Contact { name, numbers, ..old.clone() };
        if let Some(path) = &old.source {
            patch_file(path, old.card, &old, Some(&new), region)?;
        }
        self.contacts[i] = new;
        Ok(())
    }

    /// Delete a contact: its card from its file, and the file if that was its
    /// last card.
    pub fn remove(&mut self, id: &str, region: &str) -> std::io::Result<()> {
        let i = self.position(id)?;
        let gone = self.contacts[i].clone();
        if let Some(path) = &gone.source {
            patch_file(path, gone.card, &gone, None, region)?;
            // The cards after it in the file move up one.
            for c in self.contacts.iter_mut().filter(|c| c.source.as_deref() == Some(path) && c.card > gone.card) {
                c.card -= 1;
            }
        }
        self.contacts.remove(i);
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
        let r = b.import(&dir, first.clone(), "AU").unwrap();
        assert_eq!((r.added, r.merged, r.unchanged), (3, 0, 0));
        let again = b.import(&dir, first, "AU").unwrap();
        assert_eq!((again.added, again.merged, again.unchanged), (0, 0, 3), "the same phonebook twice changes nothing");
        let extra =
            parse("BEGIN:VCARD\nFN:Alex Example\nTEL:0491570199\nEMAIL:alex@example.com\nPHOTO:xyz\nEND:VCARD\n", "AU");
        let r = b.import(&dir, extra, "AU").unwrap();
        assert_eq!(r.merged, 1);
        let alex = b.contacts.iter().find(|c| c.name == "Alex Example").unwrap();
        assert_eq!(alex.numbers.len(), 3, "the new number joins the old ones");
        let on_disk = std::fs::read_to_string(alex.source.as_ref().unwrap()).unwrap();
        assert!(!on_disk.contains("EMAIL") && !on_disk.contains("PHOTO"), "only names and numbers are kept");
        let id = alex.id();
        b.update(&id, "Alex E.", vec![Phone { number: "+61491570156".into(), kind: "cell".into() }], "AU").unwrap();
        assert_eq!(Book::load(&dir, "AU").get(&id).unwrap().name, "Alex E.");
        b.remove(&id, "AU").unwrap();
        assert!(Book::load(&dir, "AU").get(&id).is_none());

        // A file of several cards: changing one keeps the rest.
        let many = dir.join("export.vcf");
        std::fs::write(&many, SAMPLE).unwrap();
        let mut b = Book::load(&dir, "AU");
        let folded = b.contacts.iter().find(|c| c.name == "Folded Name").unwrap().id();
        assert_eq!(folded, "export#2");
        b.remove("export#1", "AU").unwrap();
        let after = Book::load(&dir, "AU");
        assert!(after.contacts.iter().any(|c| c.name == "Folded Name"), "the others in the file stay");
        assert!(!after.contacts.iter().any(|c| c.name == "Sam Sample" && c.source.as_deref() == Some(&*many)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A card with more than a phone reads: an email, an address, a photo (folded
    /// over lines), a note, and a group label on a number.
    const RICH: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Alex Example\r\nN:Example;Alex;;;\r\nEMAIL:alex@example.com\r\nADR;TYPE=HOME:;;1 Main St;Town;;2000;AU\r\nPHOTO;ENCODING=b;TYPE=JPEG:QUJD\r\n REVG\r\nitem1.TEL;TYPE=CELL:0491 570 156\r\nitem1.X-ABLabel:Mobile\r\nNOTE:Met at the market\r\nEND:VCARD\r\nBEGIN:VCARD\r\nVERSION:3.0\r\nEMAIL:nobody@example.com\r\nEND:VCARD\r\nBEGIN:VCARD\r\nVERSION:3.0\r\nFN:Sam Sample\r\nTEL:0491570157\r\nEND:VCARD\r\n";

    #[test]
    fn edits_keep_the_rest_of_the_card_and_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rich.vcf");
        std::fs::write(&path, RICH).unwrap();
        let mut b = Book::load(dir.path(), "AU");
        assert_eq!(b.contacts.len(), 2, "the card with only an email is not a contact");
        // A number added: the old TEL line stays as it was, the new one joins it.
        let numbers = vec![
            Phone { number: "+61491570156".into(), kind: "mobile".into() },
            Phone { number: "+61491570199".into(), kind: "home".into() },
        ];
        b.update("rich", "Alex Example", numbers, "AU").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        for kept in [
            "EMAIL:alex@example.com",
            "ADR;TYPE=HOME:;;1 Main St;Town;;2000;AU",
            "PHOTO;ENCODING=b;TYPE=JPEG:QUJD\r\n REVG",
            "item1.TEL;TYPE=CELL:0491 570 156",
            "NOTE:Met at the market",
            "N:Example;Alex;;;",
            "EMAIL:nobody@example.com",
            "FN:Sam Sample\r\nTEL:0491570157",
        ] {
            assert!(text.contains(kept), "{kept} is kept in\n{text}");
        }
        assert!(text.contains("TEL;TYPE=HOME:+61491570199"));
        // Renamed: only the name lines change.
        b.update("rich", "Alex, the Example", vec![Phone { number: "+61491570199".into(), kind: "home".into() }], "AU")
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("FN:Alex\\, the Example") && !text.contains("N:Example;Alex"));
        assert!(!text.contains("0491 570 156"), "a number taken off goes");
        assert!(text.contains("EMAIL:alex@example.com") && text.contains("NOTE:Met at the market"));
        let again = Book::load(dir.path(), "AU");
        assert_eq!(again.get("rich").unwrap().name, "Alex, the Example");
        // Removing a card keeps the card with no name, and the file with it.
        b.remove("rich", "AU").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("Alex") && text.contains("EMAIL:nobody@example.com") && text.contains("Sam Sample"));
        assert_eq!(Book::load(dir.path(), "AU").get("rich").unwrap().name, "Sam Sample", "Sam moved up");
        assert_eq!(b.get("rich").unwrap().name, "Sam Sample", "and so here");
        // Files are the owner's alone.
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let leftovers = std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".tmp"));
        assert_eq!(leftovers.count(), 0, "no temporary files left");
    }

    #[test]
    fn a_card_changed_on_disk_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("two.vcf");
        std::fs::write(&path, SAMPLE).unwrap();
        let mut b = Book::load(dir.path(), "AU");
        // Someone else removes the first card: what was card 1 is now card 0.
        let text = std::fs::read_to_string(&path).unwrap();
        let rest = &text[text.find("END:VCARD\r\n").unwrap() + 11..];
        std::fs::write(&path, rest).unwrap();
        assert!(b.remove("two#1", "AU").is_err(), "card 1 is now Folded Name, not Sam Sample");
        assert!(b.update("two", "Changed", Vec::new(), "AU").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), rest, "nothing written");
    }

    #[test]
    fn names_with_no_letters_still_get_a_file() {
        assert_eq!(file_name("李雷", 1), "contact-1.vcf");
        assert_eq!(file_name("🙂", 2), "contact-2.vcf");
        assert_eq!(file_name("Jo Doe", 1), "jo-doe.vcf");
        assert_eq!(file_name("Jo Doe", 3), "jo-doe-3.vcf");
        let dir = tempfile::tempdir().unwrap();
        let mut b = Book::default();
        let c = Contact {
            name: "李雷".into(),
            numbers: vec![Phone { number: "+61491570159".into(), kind: "mobile".into() }],
            source: None,
            card: 0,
        };
        b.add(dir.path(), c.clone()).unwrap();
        b.add(dir.path(), c).unwrap();
        assert!(dir.path().join("contact-1.vcf").exists() && dir.path().join("contact-2.vcf").exists());
        assert_eq!(Book::load(dir.path(), "AU").contacts.len(), 2, "both are read back");
        assert_eq!(std::fs::metadata(dir.path().join("contact-1.vcf")).unwrap().permissions().mode() & 0o777, 0o600);
        let sub = dir.path().join("book");
        Book::default().add(&sub, b.contacts[0].clone()).unwrap();
        assert_eq!(std::fs::metadata(&sub).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn names_are_cleaned_and_capped() {
        let long = "A".repeat(500);
        let text = format!("BEGIN:VCARD\nFN:Bank\u{202e}knaB\u{200b}\nTEL:0491570156\nEND:VCARD\nBEGIN:VCARD\nFN:{long}\nEND:VCARD\nBEGIN:VCARD\nN:Doe\\nX;Jo\nEND:VCARD\n");
        let cards = parse(&text, "AU");
        assert_eq!(cards[0].name, "BankknaB", "bidi overrides and hidden characters go");
        assert_eq!(cards[1].name.chars().count(), NAME_MAX);
        assert_eq!(cards[2].name, "Jo Doe X", "one line");
        // A newline in a name cannot add a property to the card it is written to.
        let c = Contact { name: "Bob\nTEL:+19005550100".into(), numbers: Vec::new(), source: None, card: 0 };
        let written = write(&c);
        assert!(!written.contains("\nTEL:+1900"));
        assert!(parse(&written, "AU")[0].numbers.is_empty());
    }
}
