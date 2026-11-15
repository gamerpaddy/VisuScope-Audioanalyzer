//! Interface languages. The text in the source is English and is the key: `tr("Measure")` gives the text in the chosen
//! language, or the English text itself when the table has no entry. German is built in; any other language is a file
//! `lang_<name>.txt` next to the settings file, one `English<TAB>translation` pair per line (`\n` for a line break, `\\` for a backslash, lines
//! that start with `#` are comments). Keys of settings files, script commands and file formats are never translated.

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

static ON: AtomicBool = AtomicBool::new(false);
static TABLE: RwLock<Option<HashMap<&'static str, &'static str>>> = RwLock::new(None);
static NAME: RwLock<String> = RwLock::new(String::new());
const GERMAN: &str = include_str!("lang_de.txt");

/// The text in the chosen language.
pub fn tr(s: &'static str) -> &'static str {
    if !ON.load(Relaxed) {
        return s;
    }
    TABLE.read().as_ref().and_then(|t| t.get(s).copied()).unwrap_or(s)
}

/// Like `tr` for text that is not a literal (a name from a table).
pub fn tr_s(s: &str) -> String {
    if !ON.load(Relaxed) {
        return s.to_string();
    }
    TABLE.read().as_ref().and_then(|t| t.get(s).copied()).unwrap_or(s).to_string()
}

/// The translation, when there is one.
pub fn lookup(s: &str) -> Option<&'static str> {
    if !ON.load(Relaxed) {
        return None;
    }
    TABLE.read().as_ref().and_then(|t| t.get(s).copied())
}

pub fn parse(text: &str) -> HashMap<&'static str, &'static str> {
    let un = |s: &str| -> &'static str {
        let mut out = String::with_capacity(s.len());
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            match (c, it.peek()) {
                ('\\', Some('n')) => {
                    it.next();
                    out.push('\n');
                }
                ('\\', Some('\\')) => {
                    it.next();
                    out.push('\\');
                }
                _ => out.push(c),
            }
        }
        Box::leak(out.into_boxed_str())
    };
    text.lines().filter(|l| !l.starts_with('#')).filter_map(|l| l.split_once('\t')).filter(|(a, b)| !a.is_empty() && !b.trim().is_empty()).map(|(a, b)| (un(a), un(b))).collect()
}

fn dir() -> std::path::PathBuf {
    crate::app::settings_path().parent().map(|p| p.to_path_buf()).unwrap_or_default()
}

/// "English", "Deutsch" and the names of the language files in the settings folder.
pub fn available() -> Vec<String> {
    let mut v = vec!["English".to_string(), "Deutsch".to_string()];
    if let Ok(rd) = std::fs::read_dir(dir()) {
        let mut extra: Vec<String> = rd.flatten().filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_prefix("lang_")).and_then(|n| n.strip_suffix(".txt")).map(|n| n.to_string())).filter(|n| !v.contains(n)).collect();
        extra.sort();
        v.extend(extra);
    }
    v
}

pub fn current() -> String {
    let n = NAME.read().clone();
    if n.is_empty() { "English".into() } else { n }
}

/// Switch the language (takes effect with the next frame). An unknown name gives English.
pub fn set(name: &str) {
    let table = match name {
        "English" | "" => None,
        "Deutsch" => Some(parse(GERMAN)),
        n if n.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') => std::fs::read_to_string(dir().join(format!("lang_{n}.txt"))).ok().map(|t| parse(&t)),
        _ => None,
    };
    ON.store(table.is_some(), Relaxed);
    *NAME.write() = if table.is_some() { name.to_string() } else { String::new() };
    *TABLE.write() = table;
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_german_table_is_well_formed() {
        let t = super::parse(super::GERMAN);
        assert!(t.len() > 100, "{}", t.len());
        assert_eq!(super::parse("# c\nA\\nB\tC\\nD\nbroken line\n").get("A\nB"), Some(&"C\nD"));
        // a pair of braces in the English text is a place for a value: the translation must keep it
        for (k, v) in &t {
            assert_eq!(k.matches("{}").count(), v.matches("{}").count(), "{k}");
        }
    }
}
