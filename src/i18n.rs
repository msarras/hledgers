//! Translations of hledger's report text (Hledger.Utils.I18n): gettext PO
//! catalogs, built in or in the user's config directory, chosen by --lang,
//! and looked up by English text, optionally with a context, with `{name}`
//! placeholders filled in after lookup and plural forms chosen by count.

use std::collections::BTreeMap;

/// A language's translations (Translations): messages by English text (or
/// `context\x04text`), plural forms by English singular, and the rule that
/// picks a plural form for a count.
#[derive(Clone, Default)]
pub struct Translations {
    pub lang: String,
    messages: BTreeMap<String, String>,
    plurals: BTreeMap<String, Vec<String>>,
    plural_forms: Option<PluralExpr>,
}

impl std::fmt::Debug for Translations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Translations {:?}", self.lang)
    }
}

impl PartialEq for Translations {
    fn eq(&self, other: &Translations) -> bool {
        self.lang == other.lang && self.messages == other.messages && self.plurals == other.plurals
    }
}

impl Eq for Translations {}

impl Translations {
    /// English: every lookup returns its argument (noTranslations).
    pub fn none() -> Translations {
        Translations {
            lang: "en".to_string(),
            ..Translations::default()
        }
    }

    /// The translation of this English text, or the text (tr).
    pub fn tr<'a>(&'a self, s: &'a str) -> &'a str {
        self.messages.get(s).map_or(s, String::as_str)
    }

    /// The translation of a text used in more than one sense, with the
    /// context telling which (trc).
    pub fn trc<'a>(&'a self, ctx: &str, s: &'a str) -> &'a str {
        self.messages
            .get(&format!("{ctx}\x04{s}"))
            .map_or(s, String::as_str)
    }

    /// The translation of a template, its placeholders filled in (trf).
    pub fn trf(&self, s: &str, params: &[(&str, &str)]) -> String {
        substitute_placeholders(params, self.tr(s))
    }

    /// The plural form of a count phrase the count selects, translated,
    /// with `{n}` filled in (trn).
    pub fn trn(&self, n: i64, singular: &str, plural: &str) -> String {
        let english = if n == 1 { singular } else { plural };
        let form = self.plurals.get(singular).and_then(|forms| {
            let i = self
                .plural_forms
                .as_ref()
                .map_or(if n == 1 { 0 } else { 1 }, |e| e.eval(n));
            forms
                .get(usize::try_from(i).ok()?)
                .filter(|f| !f.is_empty())
        });
        substitute_placeholders(
            &[("n", &n.to_string())],
            form.map_or(english, String::as_str),
        )
    }

    /// The full and abbreviated month names, translated with the contexts
    /// "month" and "month abbrev" (trTimeLocale).
    pub fn month_names(&self) -> [(String, String); 12] {
        const FULL: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        const ABBREV: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        std::array::from_fn(|i| {
            (
                self.trc("month", FULL[i]).to_string(),
                self.trc("month abbrev", ABBREV[i]).to_string(),
            )
        })
    }

    /// This catalog with another's entries over it (mergeTranslations).
    fn merged(mut self, over: Translations) -> Translations {
        self.lang = over.lang;
        self.messages.extend(over.messages);
        self.plurals.extend(over.plurals);
        self.plural_forms = over.plural_forms.or(self.plural_forms);
        self
    }
}

/// The parts of a template: literal text and `{name}` placeholders
/// (scanPlaceholders).
fn scan_placeholders(s: &str) -> Vec<Result<&str, &str>> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find('{') {
        let after = &rest[i + 1..];
        let name_end = after
            .find(|c: char| c == '}' || c == '{' || !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        if name_end > 0 && after[name_end..].starts_with('}') {
            out.push(Ok(&rest[..i]));
            out.push(Err(&after[..name_end]));
            rest = &after[name_end + 1..];
        } else {
            out.push(Ok(&rest[..=i]));
            rest = after;
        }
    }
    if !rest.is_empty() {
        out.push(Ok(rest));
    }
    out
}

/// A template with each `{name}` given a value replaced by it; others are
/// left, and values are not scanned again (substitutePlaceholders).
pub fn substitute_placeholders(params: &[(&str, &str)], s: &str) -> String {
    scan_placeholders(s)
        .into_iter()
        .map(|p| match p {
            Ok(text) => text.to_string(),
            Err(name) => params
                .iter()
                .find(|(n, _)| *n == name)
                .map_or_else(|| format!("{{{name}}}"), |(_, v)| v.to_string()),
        })
        .collect()
}

/// The placeholder names in a template (placeholders).
fn placeholders(s: &str) -> std::collections::BTreeSet<&str> {
    scan_placeholders(s)
        .into_iter()
        .filter_map(Result::err)
        .collect()
}

macro_rules! catalog {
    ($lang:literal) => {
        (
            $lang,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/hledger/hledger-lib/locale/",
                $lang,
                ".po"
            )),
        )
    };
}

/// The built-in catalogs (builtinCatalogSources).
const BUILTIN_CATALOGS: [(&str, &str); 11] = [
    catalog!("de"),
    catalog!("es"),
    catalog!("fr"),
    catalog!("ga"),
    catalog!("haw"),
    catalog!("it"),
    catalog!("ja"),
    catalog!("pt-BR"),
    catalog!("ru"),
    catalog!("uk"),
    catalog!("zh-Hans"),
];

/// The template catalogs are made from.
const CATALOG_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/hledger/hledger-lib/locale/hledger.pot"
));

/// The languages with a built-in catalog, and English (builtinLanguages).
fn builtin_languages() -> impl Iterator<Item = &'static str> {
    std::iter::once("en").chain(BUILTIN_CATALOGS.iter().map(|(l, _)| *l))
}

/// The directory of the user's catalogs: locale in hledger's XDG config
/// directory (translationsOverrideDir).
fn override_dir() -> String {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|d| d.starts_with('/'))
        .unwrap_or_else(|| format!("{}/.config", std::env::var("HOME").unwrap_or_default()));
    format!("{base}/hledger/locale")
}

/// The user's catalog files by language tag, from their names
/// (overrideCatalogFiles).
fn override_catalog_files() -> Vec<(String, String)> {
    let dir = override_dir();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map(|es| {
            es.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
        .into_iter()
        .filter_map(|f| {
            let stem = f.strip_suffix(".po")?;
            Some((normalize_lang_tag(stem)?, format!("{dir}/{f}")))
        })
        .collect()
}

/// The languages with a catalog, built-in or the user's, and English,
/// sorted (availableLanguages).
fn available_languages() -> Vec<String> {
    let mut langs: Vec<String> = builtin_languages().map(str::to_string).collect();
    langs.extend(override_catalog_files().into_iter().map(|(l, _)| l));
    langs.sort();
    langs.dedup();
    langs
}

/// A language's translations: its built-in catalog, with the user's over
/// it, if any (loadTranslations). Problems with the user's catalog are
/// warnings, and it is ignored, or just its translations whose
/// placeholders differ from the English text's.
fn load_translations(lang: &str, prog_args: &[String]) -> Translations {
    let builtin = BUILTIN_CATALOGS
        .iter()
        .find(|(l, _)| *l == lang)
        .and_then(|(_, src)| parse_po(lang, src).ok())
        .unwrap_or_else(|| Translations {
            lang: lang.to_string(),
            ..Translations::default()
        });
    let warn = |msg: &str| crate::console::warn(prog_args, msg);
    let Some((_, f)) = override_catalog_files()
        .into_iter()
        .find(|(l, _)| l == lang)
    else {
        return builtin;
    };
    if std::fs::metadata(&f).is_ok_and(|m| m.len() > 1024 * 1024) {
        warn(&format!(
            "ignoring translation catalog {f}: it is larger than 1 MiB"
        ));
        return builtin;
    }
    let Ok(text) = std::fs::read(&f).map(String::from_utf8) else {
        return builtin;
    };
    let Ok(text) = text else {
        warn(&format!(
            "ignoring translation catalog {f}: it is not valid UTF-8"
        ));
        return builtin;
    };
    match parse_po(lang, &text) {
        Err(e) => {
            warn(&format!("ignoring translation catalog {f}:\n{f}: {e}"));
            builtin
        }
        Ok(c) => {
            let (c, problems) = without_placeholder_mismatches(c);
            if !problems.is_empty() {
                let lines: Vec<String> = problems.iter().map(|p| format!("  {p}")).collect();
                warn(&format!(
                    "translation catalog {f}: ignoring translations whose {{placeholders}} differ from the English text's:\n{}",
                    lines.join("\n")
                ));
            }
            builtin.merged(c)
        }
    }
}

/// The catalog without translations whose placeholders differ from their
/// English text's (a plural form may also have `{n}`), and a description of
/// each removed (withoutPlaceholderMismatches).
fn without_placeholder_mismatches(mut t: Translations) -> (Translations, Vec<String>) {
    let msgid = |k: &str| k.rsplit('\x04').next().unwrap_or(k).to_string();
    let show = |s: &std::collections::BTreeSet<&str>| {
        if s.is_empty() {
            "none".to_string()
        } else {
            s.iter()
                .map(|p| format!("{{{p}}}"))
                .collect::<Vec<_>>()
                .join(" ")
        }
    };
    let mut problems = Vec::new();
    let mut bad_plurals = Vec::new();
    for (k, vs) in &t.plurals {
        let id = msgid(k);
        let mut allowed = placeholders(&id);
        allowed.insert("n");
        let extra: std::collections::BTreeSet<&str> = vs
            .iter()
            .flat_map(|v| placeholders(v))
            .filter(|p| !allowed.contains(p))
            .collect();
        if !extra.is_empty() {
            bad_plurals.push(k.clone());
            problems.push((
                k.clone(),
                format!(
                    "{}: a plural form has {}, but only these are allowed: {}",
                    crate::text::show_string(&id),
                    show(&extra),
                    show(&allowed)
                ),
            ));
        }
    }
    let mut bad_msgs = Vec::new();
    for (k, v) in &t.messages {
        let bad = if t.plurals.contains_key(k) {
            bad_plurals.contains(k)
        } else {
            placeholders(v) != placeholders(&msgid(k))
        };
        if bad {
            bad_msgs.push(k.clone());
            if !t.plurals.contains_key(k) {
                problems.push((
                    k.clone(),
                    format!(
                        "{}: the translation has {}, but the English text has {}",
                        crate::text::show_string(&msgid(k)),
                        show(&placeholders(v)),
                        show(&placeholders(&msgid(k)))
                    ),
                ));
            }
        }
    }
    for k in bad_msgs {
        t.messages.remove(&k);
    }
    for k in bad_plurals {
        t.plurals.remove(&k);
    }
    // messages first, then plurals, each in key order
    problems.sort_by_key(|(k, p)| (p.contains(": a plural form has"), k.clone()));
    (t, problems.into_iter().map(|(_, p)| p).collect())
}

/// The translations a --lang option selects (translationsForLangOption):
/// English without one, or for C or POSIX; with `auto`, the first available
/// language the environment prefers; otherwise the language named, which
/// must have a catalog.
pub fn translations_for_lang_option(
    lang: Option<&str>,
    prog_args: &[String],
) -> Result<Translations, String> {
    let Some(s) = lang else {
        return Ok(Translations::none());
    };
    if s == "auto" {
        let env: Vec<(String, String)> = std::env::vars().collect();
        let prefs = lang_prefs_from_env(&env);
        return Ok(resolve_lang(&available_languages(), &prefs)
            .map_or_else(Translations::none, |l| load_translations(&l, prog_args)));
    }
    if ["c", "posix"].contains(&s.to_lowercase().as_str()) {
        return Ok(Translations::none());
    }
    let available = available_languages();
    match resolve_lang(&available, &[s.to_string()]) {
        Some(l) => Ok(load_translations(&l, prog_args)),
        None => {
            let shown: Vec<String> = available.iter().map(|l| short_lang_tag(l)).collect();
            Err(crate::reports::opts::usage_error(&format!(
                "--lang: no translations are available for {} (available: {})",
                crate::text::show_string(s),
                shown.join(", ")
            )))
        }
    }
}

/// Write the catalog to start translating a language from into the user's
/// catalog directory, and say where (--write-lang, writeStartingCatalog):
/// the built-in catalog for the language, or else the template with its
/// header filled in. An existing user catalog is left alone.
pub fn write_lang(raw: &str) -> Result<String, String> {
    let tag = match normalize_lang_tag(raw) {
        None => {
            return Err(format!(
                "{} is not a language tag; try eg de, fr or pt-BR",
                crate::text::show_string(raw)
            ));
        }
        Some(t) if t == "en" => {
            return Err("English is hledger's own language, and has no catalog".to_string());
        }
        Some(t) => t,
    };
    let builtin_langs: Vec<String> = BUILTIN_CATALOGS
        .iter()
        .map(|(l, _)| l.to_string())
        .collect();
    let (tag, builtin, content) = match resolve_lang(&builtin_langs, std::slice::from_ref(&tag)) {
        Some(b) => {
            let src = BUILTIN_CATALOGS
                .iter()
                .find(|(l, _)| *l == b)
                .map_or("", |(_, s)| s);
            (b, true, src.to_string())
        }
        None => {
            let content = new_catalog(&tag);
            (tag, false, content)
        }
    };
    let dir = override_dir();
    let path = format!("{dir}/{tag}.po");
    if let Some((_, f)) = override_catalog_files()
        .into_iter()
        .find(|(l, _)| *l == tag)
    {
        return Err(format!(
            "{f} exists already; to start afresh, move it aside first"
        ));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{dir}: {e}"))?;
    std::fs::write(&path, content).map_err(|e| format!("{path}: {e}"))?;
    let which = if builtin {
        "Wrote hledger's built-in "
    } else {
        "Wrote a new, untranslated "
    };
    Ok(format!(
        "{which}{tag} catalog to {path}.\nEdit it with Poedit or a text editor; hledger --lang {} uses it from now on.\nTo share your translation, see https://hledger.org/TRANSLATING.html.\n",
        short_lang_tag(&tag)
    ))
}

/// The template with its header filled in for a language, as a translator
/// tool would (startingCatalog's newCatalog).
fn new_catalog(tag: &str) -> String {
    CATALOG_TEMPLATE
        .replace(
            "\"Plural-Forms: nplurals=INTEGER; plural=EXPRESSION;\\n\"",
            &format!("\"Plural-Forms: {}\\n\"", plural_forms_for(tag)),
        )
        .replace("\"Language: \\n\"", &format!("\"Language: {tag}\\n\""))
        .replace("#, fuzzy\nmsgid \"\"\n", "msgid \"\"\n")
        .replace(
            "# Translation template for hledger.\n",
            &format!(
                "# hledger's {tag} translations, made from its translation template.\n\
                 # Check the Plural-Forms rule below: see https://hledger.org/TRANSLATING.html\n"
            ),
        )
}

/// The plural rule to start a language's catalog with (pluralFormsFor).
fn plural_forms_for(tag: &str) -> &'static str {
    let lang = tag.split('-').next().unwrap_or(tag);
    let rule = |l: &str| -> Option<&'static str> {
        Some(match l {
            "id" | "ja" | "ko" | "th" | "vi" | "zh" => "nplurals=1; plural=0;",
            "fr" | "pt-BR" => "nplurals=2; plural=(n > 1);",
            "be" | "bs" | "hr" | "ru" | "sr" | "uk" => {
                "nplurals=3; plural=(n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);"
            }
            "cs" | "sk" => "nplurals=3; plural=(n==1) ? 0 : (n>=2 && n<=4) ? 1 : 2;",
            "pl" => {
                "nplurals=3; plural=(n==1 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);"
            }
            "lt" => {
                "nplurals=3; plural=(n%10==1 && n%100!=11 ? 0 : n%10>=2 && (n%100<10 || n%100>=20) ? 1 : 2);"
            }
            "lv" => "nplurals=3; plural=(n%10==1 && n%100!=11 ? 0 : n != 0 ? 1 : 2);",
            "ro" => "nplurals=3; plural=(n==1 ? 0 : (n==0 || (n%100 > 0 && n%100 < 20)) ? 1 : 2);",
            "ga" => "nplurals=3; plural=(n==1 ? 0 : n==2 ? 1 : 2);",
            "sl" => {
                "nplurals=4; plural=(n%100==1 ? 0 : n%100==2 ? 1 : n%100==3 || n%100==4 ? 2 : 3);"
            }
            "ar" => {
                "nplurals=6; plural=(n==0 ? 0 : n==1 ? 1 : n==2 ? 2 : n%100>=3 && n%100<=10 ? 3 : n%100>=11 ? 4 : 5);"
            }
            _ => return None,
        })
    };
    rule(tag)
        .or_else(|| rule(lang))
        .unwrap_or("nplurals=2; plural=(n != 1);")
}

// PO catalogs

/// A catalog entry (PoEntry).
#[derive(Default)]
struct PoEntry {
    flags: Vec<String>,
    ctx: Option<String>,
    id: String,
    id_plural: Option<String>,
    str_: String,
    strs: Vec<(usize, String)>,
}

/// Parse a catalog in PO format (parsePo): the header, comments, flags,
/// obsolete entries, contexts, plural forms, multi-line strings, C escapes,
/// a byte order mark and CRLF line endings. Fuzzy entries are skipped and
/// empty translations count as missing; duplicates and a charset other
/// than UTF-8 are errors.
pub fn parse_po(lang: &str, input: &str) -> Result<Translations, String> {
    let text = input.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let entries = PoParser { s: &text, pos: 0 }.entries()?;
    build_translations(lang, entries)
}

struct PoParser<'a> {
    s: &'a str,
    pos: usize,
}

impl PoParser<'_> {
    fn rest(&self) -> &str {
        &self.s[self.pos..]
    }

    fn space(&mut self) {
        let r = self.rest();
        self.pos += r.len() - r.trim_start().len();
    }

    fn hspace(&mut self) {
        let r = self.rest();
        self.pos += r.len() - r.trim_start_matches([' ', '\t']).len();
    }

    fn line_col(&self) -> String {
        let before = &self.s[..self.pos];
        let line = before.matches('\n').count() + 1;
        let col = before.len() - before.rfind('\n').map_or(0, |i| i + 1) + 1;
        format!("{line}:{col}")
    }

    fn fail<T>(&self, msg: &str) -> Result<T, String> {
        Err(format!("{}: {msg}", self.line_col()))
    }

    fn entries(&mut self) -> Result<Vec<PoEntry>, String> {
        let mut out = Vec::new();
        loop {
            self.space();
            if self.rest().is_empty() {
                return Ok(out);
            }
            let mut flags = Vec::new();
            let mut comments = 0;
            while self.rest().starts_with('#') {
                comments += 1;
                let line_end = self.rest().find('\n').unwrap_or(self.rest().len());
                let line = &self.rest()[1..line_end];
                if let Some(fs) = line.strip_prefix(',') {
                    flags.extend(fs.split(',').map(|f| f.trim().to_string()));
                }
                self.pos += line_end;
                self.space();
            }
            match self.entry()? {
                None if comments == 0 => return self.fail("expected a comment line or a msgid"),
                None => {}
                Some(mut e) => {
                    e.flags = flags;
                    out.push(e);
                }
            }
        }
    }

    /// A keyword, not followed by a name character or `[`, and spaces.
    fn keyword(&mut self, k: &str) -> bool {
        let r = self.rest();
        let ok = r.starts_with(k)
            && !r[k.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '[');
        if ok {
            self.pos += k.len();
            self.hspace();
        }
        ok
    }

    fn entry(&mut self) -> Result<Option<PoEntry>, String> {
        let mut e = PoEntry::default();
        let has_ctx = self.keyword("msgctxt");
        if has_ctx {
            e.ctx = Some(self.strings()?);
        }
        if !self.keyword("msgid") {
            return if has_ctx {
                self.fail("expected msgid")
            } else {
                Ok(None)
            };
        }
        e.id = self.strings()?;
        if self.keyword("msgid_plural") {
            e.id_plural = Some(self.strings()?);
            while self.rest().starts_with("msgstr[") {
                self.pos += "msgstr[".len();
                let digits = self.rest().len()
                    - self
                        .rest()
                        .trim_start_matches(|c: char| c.is_ascii_digit())
                        .len();
                let Ok(n) = self.rest()[..digits].parse() else {
                    return self.fail("expected a plural form index");
                };
                self.pos += digits;
                if !self.rest().starts_with(']') {
                    return self.fail("expected ']'");
                }
                self.pos += 1;
                self.hspace();
                let s = self.strings()?;
                e.strs.push((n, s));
            }
            if e.strs.is_empty() {
                return self.fail("expected msgstr[");
            }
        } else if self.keyword("msgstr") {
            e.str_ = self.strings()?;
        } else {
            return self.fail("expected msgstr");
        }
        Ok(Some(e))
    }

    /// One or more quoted strings, concatenated.
    fn strings(&mut self) -> Result<String, String> {
        if !self.rest().starts_with('"') {
            return self.fail("expected '\"'");
        }
        let mut out = String::new();
        while self.rest().starts_with('"') {
            out.push_str(&self.quoted()?);
            self.space();
        }
        Ok(out)
    }

    fn quoted(&mut self) -> Result<String, String> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let mut cs = self.rest().chars();
            match cs.next() {
                None | Some('\n') => return self.fail("unterminated string"),
                Some('"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some('\\') => {
                    self.pos += 1;
                    out.push(self.escape());
                }
                Some(c) => {
                    out.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    fn escape(&mut self) -> char {
        let r = self.rest();
        let Some(c) = r.chars().next() else {
            return '\u{fffd}';
        };
        let simple = match c {
            'n' => Some('\n'),
            't' => Some('\t'),
            'r' => Some('\r'),
            '\\' => Some('\\'),
            '"' => Some('"'),
            'a' => Some('\x07'),
            'b' => Some('\x08'),
            'f' => Some('\x0c'),
            'v' => Some('\x0b'),
            _ => None,
        };
        if let Some(e) = simple {
            self.pos += 1;
            return e;
        }
        let code = |digits: &str, radix| {
            u32::from_str_radix(digits, radix)
                .ok()
                .filter(|n| *n <= 0x10FFFF)
                .and_then(char::from_u32)
                .unwrap_or('\u{fffd}')
        };
        if c == 'x' {
            let n = r[1..].len()
                - r[1..]
                    .trim_start_matches(|c: char| c.is_ascii_hexdigit())
                    .len();
            if n > 0 {
                let ch = code(&r[1..1 + n], 16);
                self.pos += 1 + n;
                return ch;
            }
        }
        let n = r
            .chars()
            .take(3)
            .take_while(|c| ('0'..='7').contains(c))
            .count();
        if n > 0 {
            let ch = code(&r[..n], 8);
            self.pos += n;
            return ch;
        }
        self.pos += c.len_utf8();
        c
    }
}

/// The translations from a catalog's entries (buildTranslations).
fn build_translations(lang: &str, entries: Vec<PoEntry>) -> Result<Translations, String> {
    let is_header = |e: &PoEntry| e.id.is_empty() && e.ctx.is_none() && e.id_plural.is_none();
    let header: Vec<(String, String)> = entries
        .iter()
        .find(|e| is_header(e))
        .map(|h| {
            h.str_
                .lines()
                .filter_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    Some((k.trim().to_lowercase(), v.trim().to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    let field = |k: &str| header.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    if let Some(ct) = field("content-type") {
        let charset = ct
            .split(';')
            .map(|p| p.trim().to_lowercase())
            .find_map(|p| p.strip_prefix("charset=").map(|c| c.trim().to_string()));
        if let Some(cs) = charset.filter(|c| !["utf-8", "utf8", "charset"].contains(&c.as_str())) {
            return Err(format!(
                "unsupported charset {cs} (translation catalogs must be UTF-8)"
            ));
        }
    }
    let plural_forms = match field("plural-forms") {
        None => None,
        Some(s) => Some(
            parse_plural_forms(s)
                .ok_or_else(|| format!("could not parse the Plural-Forms header: {s}"))?,
        ),
    };
    let live: Vec<&PoEntry> = entries
        .iter()
        .filter(|e| !is_header(e) && !e.flags.iter().any(|f| f == "fuzzy"))
        .collect();
    let key = |e: &PoEntry| match &e.ctx {
        Some(c) => format!("{c}\x04{}", e.id),
        None => e.id.clone(),
    };
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for e in &live {
        *counts.entry(key(e)).or_default() += 1;
    }
    let dups: Vec<String> = (counts.iter())
        .filter(|(_, n)| **n > 1)
        .map(|(k, _)| crate::text::show_string(&k.replace('\x04', "|")))
        .collect();
    if !dups.is_empty() {
        return Err(format!("duplicate entries for: {}", dups.join(" ")));
    }
    let mut messages = BTreeMap::new();
    let mut plurals = BTreeMap::new();
    for e in live {
        let singular = match e.id_plural {
            None => e.str_.clone(),
            Some(_) => e
                .strs
                .iter()
                .find(|(i, _)| *i == 0)
                .map(|(_, s)| s.clone())
                .unwrap_or_default(),
        };
        if !singular.is_empty() {
            messages.insert(key(e), singular);
        }
        if e.id_plural.is_some() {
            let mut forms = e.strs.clone();
            forms.sort_by_key(|(i, _)| *i);
            let forms: Vec<String> = forms.into_iter().map(|(_, s)| s).collect();
            if forms.iter().any(|f| !f.is_empty()) {
                plurals.insert(key(e), forms);
            }
        }
    }
    Ok(Translations {
        lang: lang.to_string(),
        messages,
        plurals,
        plural_forms,
    })
}

// Plural forms

/// A plural rule's expression: the C subset gettext allows (PExpr).
#[derive(Clone, Debug)]
enum PluralExpr {
    N,
    Lit(i64),
    Not(Box<PluralExpr>),
    Bin(&'static str, Box<PluralExpr>, Box<PluralExpr>),
    Cond(Box<PluralExpr>, Box<PluralExpr>, Box<PluralExpr>),
}

impl PluralExpr {
    fn eval(&self, n: i64) -> i64 {
        let b = |x: bool| i64::from(x);
        match self {
            PluralExpr::N => n,
            PluralExpr::Lit(i) => *i,
            PluralExpr::Not(e) => b(e.eval(n) == 0),
            PluralExpr::Cond(c, x, y) => {
                if c.eval(n) != 0 {
                    x.eval(n)
                } else {
                    y.eval(n)
                }
            }
            PluralExpr::Bin(op, x, y) => {
                let (x, y) = (x.eval(n), y.eval(n));
                match *op {
                    "%" => {
                        if y == 0 {
                            0
                        } else {
                            x.rem_euclid(y)
                        }
                    }
                    "<" => b(x < y),
                    "<=" => b(x <= y),
                    ">" => b(x > y),
                    ">=" => b(x >= y),
                    "==" => b(x == y),
                    "!=" => b(x != y),
                    "&&" => b(x != 0 && y != 0),
                    "||" => b(x != 0 || y != 0),
                    _ => 0,
                }
            }
        }
    }
}

/// A Plural-Forms header value's rule, if its nplurals and plural parse
/// (parsePluralForms).
fn parse_plural_forms(s: &str) -> Option<PluralExpr> {
    let fields: Vec<(&str, &str)> = s
        .split(';')
        .filter_map(|p| p.split_once('=').map(|(k, v)| (k.trim(), v.trim())))
        .collect();
    let get = |k: &str| fields.iter().find(|(n, _)| *n == k).map(|(_, v)| *v);
    get("nplurals")?.parse::<i64>().ok()?;
    let mut p = ExprParser {
        s: get("plural")?,
        pos: 0,
    };
    p.space();
    let e = p.cond()?;
    p.at_end().then_some(e)
}

struct ExprParser<'a> {
    s: &'a str,
    pos: usize,
}

impl ExprParser<'_> {
    fn rest(&self) -> &str {
        &self.s[self.pos..]
    }

    fn space(&mut self) {
        let r = self.rest();
        self.pos += r.len() - r.trim_start().len();
    }

    fn at_end(&self) -> bool {
        self.rest().is_empty()
    }

    fn sym(&mut self, t: &str) -> bool {
        if self.rest().starts_with(t) {
            self.pos += t.len();
            self.space();
            true
        } else {
            false
        }
    }

    /// cond: binary, optionally `? cond : cond`.
    fn cond(&mut self) -> Option<PluralExpr> {
        let c = self.binary(0)?;
        if self.sym("?") {
            let a = self.cond()?;
            if !self.sym(":") {
                return None;
            }
            let b = self.cond()?;
            return Some(PluralExpr::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        Some(c)
    }

    /// Left-associative binary operators by precedence level, loosest
    /// first: ||, &&, equality, comparison, %.
    fn binary(&mut self, level: usize) -> Option<PluralExpr> {
        const LEVELS: [&[&str]; 5] = [
            &["||"],
            &["&&"],
            &["==", "!="],
            &["<=", ">=", "<", ">"],
            &["%"],
        ];
        if level == LEVELS.len() {
            return self.prefix();
        }
        let mut x = self.binary(level + 1)?;
        'outer: loop {
            for op in LEVELS[level] {
                if self.sym(op) {
                    let y = self.binary(level + 1)?;
                    x = PluralExpr::Bin(op, Box::new(x), Box::new(y));
                    continue 'outer;
                }
            }
            return Some(x);
        }
    }

    fn prefix(&mut self) -> Option<PluralExpr> {
        if self.rest().starts_with('!') && !self.rest().starts_with("!=") {
            self.pos += 1;
            self.space();
            return Some(PluralExpr::Not(Box::new(self.prefix()?)));
        }
        self.term()
    }

    fn term(&mut self) -> Option<PluralExpr> {
        let r = self.rest();
        if r.starts_with('n') && !r[1..].chars().next().is_some_and(char::is_alphanumeric) {
            self.pos += 1;
            self.space();
            return Some(PluralExpr::N);
        }
        let digits = r.len() - r.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 {
            let n = r[..digits].parse().ok()?;
            self.pos += digits;
            self.space();
            return Some(PluralExpr::Lit(n));
        }
        if self.sym("(") {
            let e = self.cond()?;
            return self.sym(")").then_some(e);
        }
        None
    }
}

// Language tags

/// Whether a language tag is well formed: two or three letters, then
/// subtags of one to eight letters or digits (isValidLangTag).
fn is_valid_lang_tag(t: &str) -> bool {
    let mut parts = t.split('-');
    let l = parts.next().unwrap_or("");
    (l.len() == 2 || l.len() == 3)
        && l.chars().all(|c| c.is_ascii_alphabetic())
        && parts
            .all(|s| !s.is_empty() && s.len() <= 8 && s.chars().all(|c| c.is_ascii_alphanumeric()))
        && t.len() <= 35
}

/// A language tag as found in the environment, made canonical, or None if
/// it names no language (normalizeLangTag): POSIX locale names (de_DE.UTF-8
/// gives de-DE; C and POSIX none), case, Chinese regions as scripts.
pub fn normalize_lang_tag(raw: &str) -> Option<String> {
    let value = raw.split(';').next().unwrap_or("").trim();
    let base_end = value.find(['.', '@']).unwrap_or(value.len());
    let (base, suffix) = value.split_at(base_end);
    if base.is_empty() || ["c", "posix"].contains(&base.to_lowercase().as_str()) {
        return None;
    }
    let modifier = suffix.split_once('@').map_or("", |(_, m)| m).to_lowercase();
    let base = base.replace('_', "-");
    let parts: Vec<&str> = base.split('-').filter(|p| !p.is_empty()).collect();
    let (l, rest) = parts.split_first()?;
    if !((l.chars().count() == 2 || l.chars().count() == 3) && l.chars().all(char::is_alphabetic)) {
        return None;
    }
    let mut tag = vec![l.to_lowercase()];
    match modifier.as_str() {
        "latin" => tag.push("Latn".to_string()),
        "cyrillic" => tag.push("Cyrl".to_string()),
        _ => {}
    }
    for s in rest {
        let alpha = s.chars().all(char::is_alphabetic);
        tag.push(match s.chars().count() {
            4 if alpha => {
                let lower = s.to_lowercase();
                let mut cs = lower.chars();
                cs.next()
                    .map_or(String::new(), |c| c.to_uppercase().chain(cs).collect())
            }
            2 if alpha => s.to_uppercase(),
            _ => s.to_string(),
        });
    }
    let tag = tag.join("-");
    let tag = match tag.to_lowercase().as_str() {
        "zh" | "zh-cn" | "zh-sg" | "zh-hans" => "zh-Hans".to_string(),
        "zh-tw" | "zh-hk" | "zh-mo" | "zh-hant" => "zh-Hant".to_string(),
        "no" | "no-no" => "nb".to_string(),
        _ => tag,
    };
    is_valid_lang_tag(&tag).then_some(tag)
}

/// The tags to try for a tag, most specific first (langTagCandidates).
fn lang_tag_candidates(t: &str) -> Vec<String> {
    let parts: Vec<&str> = t.split('-').collect();
    (1..=parts.len())
        .rev()
        .map(|n| parts[..n].join("-"))
        .collect()
}

/// The shortest spelling of a tag that selects it (shortLangTag).
fn short_lang_tag(t: &str) -> String {
    lang_tag_candidates(t)
        .into_iter()
        .rev()
        .find(|c| normalize_lang_tag(c).as_deref() == Some(t))
        .unwrap_or_else(|| t.to_string())
}

/// The first available language among the preferred ones, each tried with
/// its subtags dropped from the right before the next (resolveLang).
fn resolve_lang(available: &[String], prefs: &[String]) -> Option<String> {
    for p in prefs.iter().filter_map(|p| normalize_lang_tag(p)) {
        for c in lang_tag_candidates(&p) {
            if let Some(a) = available
                .iter()
                .find(|a| a.to_lowercase() == c.to_lowercase())
            {
                return Some(a.clone());
            }
        }
    }
    None
}

/// The languages the environment prefers, as gettext reads it: none if the
/// locale (LC_ALL, else LC_MESSAGES, else LANG) is unset, C or POSIX;
/// otherwise the LANGUAGE list, then the locale (langPrefsFromEnv).
fn lang_prefs_from_env(env: &[(String, String)]) -> Vec<String> {
    let get = |k: &str| {
        env.iter()
            .find(|(n, v)| n == k && !v.is_empty())
            .map(|(_, v)| v.clone())
    };
    let Some(loc) = get("LC_ALL")
        .or_else(|| get("LC_MESSAGES"))
        .or_else(|| get("LANG"))
    else {
        return Vec::new();
    };
    if normalize_lang_tag(&loc).is_none() {
        return Vec::new();
    }
    let mut prefs: Vec<String> = get("LANGUAGE")
        .map(|l| l.split(':').map(str::to_string).collect())
        .unwrap_or_default();
    prefs.push(loc);
    prefs.iter().filter_map(|p| normalize_lang_tag(p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The sample catalog of upstream's tests_I18n.
    const SAMPLE_PO: &str = "# A sample catalog.\nmsgid \"\"\nmsgstr \"\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\"Plural-Forms: nplurals=2; plural=(n != 1);\\n\"\n\nmsgid \"Balance Sheet\"\nmsgstr \"Bilanz\"\n\nmsgid \"Untranslated\"\nmsgstr \"\"\n\nmsgid \"Empty\"\nmsgstr \"\"\n\n#, fuzzy\nmsgid \"Fuzzy\"\nmsgstr \"Unscharf\"\n\n#~ msgid \"Obsolete\"\n#~ msgstr \"Veraltet\"\n\nmsgctxt \"column heading\"\nmsgid \"Total\"\nmsgstr \"Summe\"\n\nmsgid \"Multi\"\nmsgstr \"\"\n\"line one\\n\"\n\"line two \\\"quoted\\\" \\\\ \\374 \\x41\"\n\nmsgid \"{n} day\"\nmsgid_plural \"{n} days\"\nmsgstr[0] \"{n} Tag\"\nmsgstr[1] \"{n} Tage\"\n\nmsgid \"Balance changes in {period}:\"\nmsgstr \"Saldoänderungen in {period}:\"\n";

    #[test]
    fn substitute_placeholders_and_placeholders() {
        assert_eq!(substitute_placeholders(&[("a", "1")], "x {a} y"), "x 1 y");
        assert_eq!(
            substitute_placeholders(&[("a", "{b}"), ("b", "2")], "{a}{b}"),
            "{b}2"
        );
        assert_eq!(
            substitute_placeholders(&[], "{unknown} { }"),
            "{unknown} { }"
        );
        assert_eq!(substitute_placeholders(&[("a", "1")], "{a}{a}"), "11");
        let ps: Vec<&str> = placeholders("a {x} {y_1} {bad name} { {z}")
            .into_iter()
            .collect();
        assert_eq!(ps, ["x", "y_1", "z"]);
        assert!(placeholders("").is_empty());
    }

    #[test]
    fn parse_po_sample() {
        let t = parse_po("de", SAMPLE_PO).unwrap();
        assert_eq!(t.lang, "de");
        assert_eq!(t.tr("Balance Sheet"), "Bilanz");
        assert_eq!(t.tr("Untranslated"), "Untranslated");
        assert_eq!(t.tr("Empty"), "Empty");
        assert_eq!(t.tr("Fuzzy"), "Fuzzy");
        assert_eq!(t.tr("Obsolete"), "Obsolete");
        assert_eq!(t.trc("column heading", "Total"), "Summe");
        assert_eq!(t.tr("Total"), "Total");
        assert_eq!(t.tr("Multi"), "line one\nline two \"quoted\" \\ \u{fc} A");
        assert_eq!(t.trn(1, "{n} day", "{n} days"), "1 Tag");
        assert_eq!(t.trn(2, "{n} day", "{n} days"), "2 Tage");
        assert_eq!(t.trn(0, "{n} day", "{n} days"), "0 Tage");
        assert_eq!(t.tr("{n} day"), "{n} Tag");
        assert_eq!(
            t.trf("Balance changes in {period}:", &[("period", "2024")]),
            "Saldoänderungen in 2024:"
        );
    }

    #[test]
    fn parse_po_errors_and_tolerances() {
        let bad = |s: &str| assert!(parse_po("de", s).is_err(), "{s:?}");
        bad("msgid \"a\"\nmsgstr \"b\"\nmsgid \"a\"\nmsgstr \"c\"\n");
        bad("garbage\n");
        bad("msgid \"a\"\nmsgstr \"b\n");
        bad("msgid \"\"\nmsgstr \"Content-Type: text/plain; charset=ISO-8859-1\\n\"\n");
        bad("msgid \"\"\nmsgstr \"Plural-Forms: nplurals=2; plural=(n +);\\n\"\n");
        let ok = |s: &str| assert!(parse_po("de", s).is_ok(), "{s:?}");
        ok("");
        ok("# just a comment\n");
        ok("\u{feff}msgid \"a\"\r\nmsgstr \"b\"\r\n");
        ok("#~ msgid \"old\"\n#~ msgstr \"alt\"\n");
        ok("msgid \"\"\nmsgstr \"Content-Type: text/plain; charset=CHARSET\\n\"\n");
    }

    #[test]
    fn plural_rules() {
        let rule = |s: &str| {
            let e = parse_plural_forms(s);
            move |n: i64| e.as_ref().map_or(-1, |e| e.eval(n))
        };
        let en = rule("nplurals=2; plural=(n != 1);");
        let fr = rule("nplurals=2; plural=(n > 1);");
        let ru = rule(
            "nplurals=3; plural=(n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);",
        );
        let zh = rule("nplurals=1; plural=0;");
        let ar = rule(
            "nplurals=6; plural=(n==0 ? 0 : n==1 ? 1 : n==2 ? 2 : n%100>=3 && n%100<=10 ? 3 : n%100>=11 ? 4 : 5);",
        );
        let pl = rule(
            "nplurals=3; plural=(n==1 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);",
        );
        assert_eq!([0, 1, 2].map(en), [1, 0, 1]);
        assert_eq!([0, 1, 2].map(fr), [0, 0, 1]);
        assert_eq!([1, 2, 5, 11, 21, 22, 25].map(ru), [0, 1, 2, 2, 0, 1, 2]);
        assert_eq!([0, 1, 5].map(zh), [0, 0, 0]);
        assert_eq!([0, 1, 2, 3, 11, 100].map(ar), [0, 1, 2, 3, 4, 5]);
        assert_eq!([1, 2, 5, 22, 25].map(pl), [0, 1, 2, 1, 2]);
    }

    #[test]
    fn language_tags() {
        let n = |s: &str| normalize_lang_tag(s);
        assert_eq!(n("de_DE.UTF-8@euro").as_deref(), Some("de-DE"));
        assert_eq!(n("pt-br").as_deref(), Some("pt-BR"));
        assert_eq!(n("zh_CN").as_deref(), Some("zh-Hans"));
        assert_eq!(n("sr@latin").as_deref(), Some("sr-Latn"));
        assert_eq!(n("C"), None);
        assert_eq!(n(""), None);
        assert_eq!(short_lang_tag("zh-Hans"), "zh");
        assert_eq!(short_lang_tag("pt-BR"), "pt-BR");
        let avail: Vec<String> = ["de", "en", "pt-BR"].map(String::from).to_vec();
        let prefs = |ps: &[&str]| ps.iter().map(|p| p.to_string()).collect::<Vec<_>>();
        assert_eq!(
            resolve_lang(&avail, &prefs(&["de-CH", "en"])).as_deref(),
            Some("de")
        );
        assert_eq!(resolve_lang(&avail, &prefs(&["pt"])), None);
        let env = |vs: &[(&str, &str)]| -> Vec<(String, String)> {
            vs.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        assert_eq!(
            lang_prefs_from_env(&env(&[("LANGUAGE", "de"), ("LC_ALL", "C")])),
            Vec::<String>::new()
        );
        assert_eq!(
            lang_prefs_from_env(&env(&[("LANGUAGE", "fr:de"), ("LANG", "es_ES.UTF-8")])),
            ["fr", "de", "es-ES"]
        );
    }

    #[test]
    fn builtin_catalogs_parse() {
        for (lang, src) in BUILTIN_CATALOGS {
            assert!(parse_po(lang, src).is_ok(), "{lang}");
        }
    }
}
