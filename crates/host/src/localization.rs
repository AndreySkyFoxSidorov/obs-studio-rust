//! Bounded editable CSV localization; no UI types or implicit background I/O.
//! See doc/migration/localization-ui-audit.md. Loading runs on a host I/O owner.
use std::{
    borrow::Cow,
    error::Error,
    fmt,
    fs::File,
    io::{self, Read},
    mem::size_of,
    path::Path,
};

pub const MAX_LOCALIZATION_FILE_BYTES: usize = 32 * 1024 * 1024;
const MAX_LANGUAGES: usize = 128;
const MAX_ROWS: usize = 32768;
const MAX_CELL_BYTES: usize = 32768;
const MAX_RETAINED_BYTES: usize = 8 * 1024 * 1024;
const MAX_FORMAT_BYTES: usize = 32768;

include!("localization_aliases.rs");

#[derive(Debug)]
pub struct LocalizationError {
    code: &'static str,
    detail: String,
    cause: Option<io::Error>,
}
impl LocalizationError {
    pub fn code(&self) -> &'static str {
        self.code
    }
    pub fn detail(&self) -> &str {
        &self.detail
    }
}
impl fmt::Display for LocalizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}
impl Error for LocalizationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.cause.as_ref().map(|cause| cause as &dyn Error)
    }
}
fn invalid(code: &'static str, detail: impl Into<String>) -> LocalizationError {
    LocalizationError {
        code,
        detail: detail.into(),
        cause: None,
    }
}
fn io_error(cause: io::Error) -> LocalizationError {
    LocalizationError {
        code: "localization_io",
        detail: cause.to_string(),
        cause: Some(cause),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Language {
    pub code: String,
    pub name: String,
}
#[derive(Debug)]
struct Entry {
    key: String,
    english: String,
    translated: Option<String>,
}
#[derive(Debug)]
struct Alias {
    text: String,
    entry: usize,
    priority: u8,
}

#[derive(Debug)]
pub struct Localization {
    languages: Vec<Language>,
    language: String,
    entries: Vec<Entry>,
    aliases: Vec<Alias>,
    missing: usize,
    ambiguous_aliases: usize,
    retained_bytes: usize,
}
impl Localization {
    pub fn from_csv(bytes: &[u8], language: &str) -> Result<Self, LocalizationError> {
        if !valid_code(language) {
            return Err(invalid(
                "localization_language",
                "locale must contain 2..35 ASCII language-tag characters",
            ));
        }
        if bytes.len() > MAX_LOCALIZATION_FILE_BYTES {
            return Err(invalid("localization_limit", "CSV exceeds 32 MiB"));
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|cause| invalid("localization_utf8", cause.to_string()))?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut parser = Csv::new(text);
        let headers = parser
            .row()?
            .ok_or_else(|| invalid("localization_header", "CSV has no header"))?;
        if headers.first().map(String::as_str) != Some("key")
            || headers.len() < 2
            || headers.len() > MAX_LANGUAGES + 1
        {
            return Err(invalid(
                "localization_header",
                "first header must be key with 1..=128 language columns",
            ));
        }
        for (index, code) in headers.iter().enumerate().skip(1) {
            if !valid_code(code) {
                return Err(invalid(
                    "localization_header",
                    format!("invalid locale header {code:?}"),
                ));
            }
            if headers[..index].contains(code) {
                return Err(invalid(
                    "localization_header",
                    format!("duplicate locale header {code:?}"),
                ));
            }
        }
        let english_column = headers
            .iter()
            .position(|code| code == "en-US")
            .ok_or_else(|| invalid("localization_header", "en-US fallback column is required"))?;
        let selected_column = headers
            .iter()
            .position(|code| code == language)
            .filter(|index| *index != 0)
            .ok_or_else(|| {
                invalid(
                    "localization_language",
                    format!("unsupported locale {language:?}"),
                )
            })?;
        let mut budget = Budget(0);
        budget.charge(size_of::<Self>())?;
        budget.charge((headers.len() - 1) * size_of::<Language>())?;
        let mut languages = Vec::new();
        reserve(&mut languages, headers.len() - 1)?;
        for code in headers.iter().skip(1) {
            budget.charge(code.len() * 2)?;
            languages.push(Language {
                code: exact(code.clone()),
                name: exact(code.clone()),
            });
        }
        budget.charge(language.len())?;
        let language = exact(language.to_owned());
        let mut entries: Vec<Entry> = Vec::new();
        let mut missing = 0;
        while let Some(mut row) = parser.row()? {
            if entries.len() >= MAX_ROWS {
                return Err(invalid("localization_limit", "CSV exceeds 32768 keys"));
            }
            if row.len() != headers.len() {
                return Err(invalid(
                    "localization_columns",
                    format!(
                        "row {} has {} columns; expected {}",
                        parser.row_number,
                        row.len(),
                        headers.len()
                    ),
                ));
            }
            if row[0].is_empty() {
                return Err(invalid(
                    "localization_key",
                    format!("row {} has an empty key", parser.row_number),
                ));
            }
            if row[0] == "SkyLive.LanguageName" {
                for (entry, name) in languages.iter_mut().zip(row.iter().skip(1)) {
                    if !name.is_empty() {
                        budget.release(entry.name.len());
                        budget.charge(name.len())?;
                        entry.name = exact(name.clone());
                    }
                }
            }
            missing += usize::from(row[selected_column].is_empty());
            let key = std::mem::take(&mut row[0]);
            let english = std::mem::take(&mut row[english_column]);
            let translated = if selected_column == english_column || row[selected_column].is_empty()
            {
                None
            } else {
                Some(std::mem::take(&mut row[selected_column]))
            };
            if key.starts_with("SkyLive.") {
                if let Some(text) = translated.as_deref() {
                    if rust_fields(&english) != rust_fields(text) {
                        return Err(invalid(
                            "localization_format",
                            format!("Rust placeholders differ for key {key:?}"),
                        ));
                    }
                }
            }
            budget
                .charge(key.len() + english.len() + translated.as_ref().map_or(0, String::len))?;
            if entries.len() == entries.capacity() {
                let capacity = (entries.capacity().max(1) * 2).min(MAX_ROWS);
                budget.charge((capacity - entries.capacity()) * size_of::<Entry>())?;
                reserve(&mut entries, capacity)?;
            }
            entries.push(Entry {
                key,
                english,
                translated,
            });
        }
        entries.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        for pair in entries.windows(2) {
            if pair[0].key == pair[1].key {
                return Err(invalid(
                    "localization_key",
                    format!("duplicate key {:?}", pair[0].key),
                ));
            }
        }
        // Shrinking removes input-derived unused capacity from the read model.
        budget.release((entries.capacity() - entries.len()) * size_of::<Entry>());
        let entries = entries.into_boxed_slice().into_vec();
        budget.charge(entries.len() * size_of::<Alias>())?;
        let mut aliases = Vec::new();
        reserve(&mut aliases, entries.len())?;
        for (index, entry) in entries.iter().enumerate() {
            // Camera driver labels use explicit keys; they must not replace
            // generic English aliases such as audio Pan.
            if entry.key.starts_with("SkyLive.CameraControl.") { continue; }
            if entry.english.is_empty() {
                continue;
            }
            let normalized = alias_text(&entry.english);
            if normalized.is_empty() {
                continue;
            }
            budget.charge(normalized.len())?;
            aliases.push(Alias {
                text: exact(normalized.into_owned()),
                entry: index,
                priority: alias_priority(&entry.key),
            });
        }
        aliases.sort_unstable_by(|a, b| {
            a.text
                .cmp(&b.text)
                .then(a.priority.cmp(&b.priority))
                .then(entries[a.entry].key.cmp(&entries[b.entry].key))
        });
        let mut ambiguous_aliases = 0;
        let mut index = 0;
        while index < aliases.len() {
            let first = index;
            index += 1;
            while index < aliases.len() && aliases[index].text == aliases[first].text {
                index += 1;
            }
            ambiguous_aliases += usize::from(index - first > 1);
        }
        // The first item wins: frontend, SkyLive, module; then lexical key.
        aliases.dedup_by(|a, b| a.text == b.text);
        let aliases = aliases.into_boxed_slice().into_vec();
        let languages = languages.into_boxed_slice().into_vec();
        let retained_bytes = size_of::<Self>()
            + language.capacity()
            + languages.capacity() * size_of::<Language>()
            + languages
                .iter()
                .map(|value| value.code.capacity() + value.name.capacity())
                .sum::<usize>()
            + entries.capacity() * size_of::<Entry>()
            + entries
                .iter()
                .map(|entry| {
                    entry.key.capacity()
                        + entry.english.capacity()
                        + entry.translated.as_ref().map_or(0, String::capacity)
                })
                .sum::<usize>()
            + aliases.capacity() * size_of::<Alias>()
            + aliases
                .iter()
                .map(|alias| alias.text.capacity())
                .sum::<usize>();
        if retained_bytes > MAX_RETAINED_BYTES {
            return Err(invalid(
                "localization_limit",
                "retained catalog exceeds 8 MiB",
            ));
        }
        Ok(Self {
            languages,
            language,
            entries,
            aliases,
            missing,
            ambiguous_aliases,
            retained_bytes,
        })
    }
    /// The owned input snapshot is limited to 32 MiB plus one over-limit sentinel.
    pub fn load(path: &Path, language: &str) -> Result<Self, LocalizationError> {
        let mut file = File::open(path).map_err(io_error)?;
        let expected = file.metadata().map_err(io_error)?.len();
        if expected > MAX_LOCALIZATION_FILE_BYTES as u64 {
            return Err(invalid("localization_limit", "CSV file exceeds 32 MiB"));
        }
        // One exact staging allocation, including a growth sentinel. A changing
        // external file is rejected rather than expanding Vec capacity in read_to_end.
        let expected = expected as usize;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(expected + 1)
            .map_err(|error| invalid("localization_limit", error.to_string()))?;
        bytes.resize(expected + 1, 0);
        let mut length = 0;
        while length < bytes.len() {
            match file.read(&mut bytes[length..]) {
                Ok(0) => break,
                Ok(count) => length += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(io_error(error)),
            }
        }
        if length != expected || file.metadata().map_err(io_error)?.len() != expected as u64 {
            return Err(invalid(
                "localization_changed",
                "CSV changed size while being read; reload again",
            ));
        }
        Self::from_csv(&bytes[..length], language)
    }
    pub fn embedded(language: &str) -> Result<Self, LocalizationError> {
        Self::from_csv(
            include_bytes!("../../../localization/translations.csv"),
            language,
        )
    }
    pub fn languages(&self) -> &[Language] {
        &self.languages
    }
    pub fn language(&self) -> &str {
        &self.language
    }
    pub fn missing(&self) -> usize {
        self.missing
    }
    pub fn ambiguous_aliases(&self) -> usize {
        self.ambiguous_aliases
    }
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
    pub fn key_count(&self) -> usize {
        self.entries.len()
    }
    pub fn translate<'a>(&'a self, key: &'a str) -> &'a str {
        let Ok(index) = self
            .entries
            .binary_search_by(|entry| entry.key.as_str().cmp(key))
        else {
            return key;
        };
        let entry = &self.entries[index];
        entry
            .translated
            .as_deref()
            .filter(|value| !value.is_empty())
            .or_else(|| (!entry.english.is_empty()).then_some(entry.english.as_str()))
            .unwrap_or(key)
    }
    /// Existing English widget contracts remain exact for English and fallbacks.
    /// Explicit semantic-key label with Qt mnemonic markers adapted for egui.
    pub fn label<'a>(&'a self, key: &'a str) -> Cow<'a, str> {
        qt_text(self.translate(key))
    }

    pub fn text<'a>(&'a self, english: &'a str) -> Cow<'a, str> {
        if english.len() > MAX_CELL_BYTES {
            return Cow::Borrowed(english);
        }
        // Prefer an already plain literal such as R&D over interpreting its &
        // as another Qt accelerator. Explicit Qt markup is the second lookup.
        let literal = without_ellipsis(english);
        let normalized = alias_text(english);
        // Widget literals identify original source keys, independently of an
        // editable English cell. Keep plain punctuation ahead of Qt markup.
        let original = ORIGINAL_ALIASES
            .binary_search_by(|(text, _, _)| text.cmp(&literal))
            .ok()
            .or_else(|| {
                ORIGINAL_ALIASES
                    .binary_search_by(|(text, _, _)| text.cmp(&normalized.as_ref()))
                    .ok()
            });
        if let Some(index) = original {
            let (_, key, canonical) = ORIGINAL_ALIASES[index];
            if let Ok(index) = self
                .entries
                .binary_search_by(|entry| entry.key.as_str().cmp(key))
            {
                let entry = &self.entries[index];
                return match entry.translated.as_deref() {
                    Some(value) => qt_text(value),
                    None if entry.english != canonical => qt_text(&entry.english),
                    None => Cow::Borrowed(english),
                };
            }
        }
        let Some(index) = self
            .aliases
            .binary_search_by(|alias| alias.text.as_str().cmp(literal))
            .ok()
            .or_else(|| {
                self.aliases
                    .binary_search_by(|alias| alias.text.as_str().cmp(normalized.as_ref()))
                    .ok()
            })
        else {
            return Cow::Borrowed(english);
        };
        match self.entries[self.aliases[index].entry]
            .translated
            .as_deref()
        {
            Some(value) => qt_text(value),
            None => Cow::Borrowed(english),
        }
    }
    /// Maximal digit runs identify positional arguments: %1 never eats %10.
    /// Percent signs without digits are literal; inserted arguments are not rescanned.
    pub fn format(&self, key: &str, args: &[&str]) -> Result<String, LocalizationError> {
        if key.len() > MAX_CELL_BYTES {
            return Err(invalid(
                "localization_limit",
                "format key exceeds 32768 bytes",
            ));
        }
        if let Ok(index) = self
            .entries
            .binary_search_by(|entry| entry.key.as_str().cmp(key))
        {
            // A translator omitting a placeholder must not hide a caller's
            // missing source-format argument.
            validate_arguments(&self.entries[index].english, args.len(), key)?;
        }
        let source = self.translate(key);
        let bytes = source.as_bytes();
        let mut output = String::new();
        let mut start = 0;
        let mut index = 0;
        while index < bytes.len() {
            let Some((next, argument)) = placeholder(bytes, index)? else {
                index += 1;
                continue;
            };
            append(&mut output, &source[start..index])?;
            index = next;
            let value = argument
                .checked_sub(1)
                .and_then(|index| args.get(index))
                .ok_or_else(|| {
                    invalid(
                        "localization_placeholder",
                        format!("missing argument %{argument} for key {key:?}"),
                    )
                })?;
            append(&mut output, value)?;
            start = index;
        }
        append(&mut output, &source[start..])?;
        Ok(output)
    }
}
fn append(output: &mut String, value: &str) -> Result<(), LocalizationError> {
    if output
        .len()
        .checked_add(value.len())
        .is_none_or(|length| length > MAX_FORMAT_BYTES)
    {
        return Err(invalid(
            "localization_limit",
            "formatted text exceeds 32768 bytes",
        ));
    }
    output
        .try_reserve(value.len())
        .map_err(|_| invalid("localization_limit", "formatted text allocation"))?;
    output.push_str(value);
    Ok(())
}
fn validate_arguments(
    source: &str,
    argument_count: usize,
    key: &str,
) -> Result<(), LocalizationError> {
    let bytes = source.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let Some((next, argument)) = placeholder(bytes, index)? else {
            index += 1;
            continue;
        };
        index = next;
        if argument == 0 || argument > argument_count {
            return Err(invalid(
                "localization_placeholder",
                format!("missing source argument %{argument} for key {key:?}"),
            ));
        }
    }
    Ok(())
}
fn placeholder(
    bytes: &[u8],
    mut index: usize,
) -> Result<Option<(usize, usize)>, LocalizationError> {
    if bytes[index] != b'%' || !bytes.get(index + 1).is_some_and(u8::is_ascii_digit) {
        return Ok(None);
    }
    index += 1;
    let mut argument = 0usize;
    while let Some(digit) = bytes.get(index).filter(|digit| digit.is_ascii_digit()) {
        argument = argument
            .checked_mul(10)
            .and_then(|value| value.checked_add(usize::from(*digit - b'0')))
            .ok_or_else(|| {
                invalid(
                    "localization_placeholder",
                    "positional placeholder index overflow",
                )
            })?;
        index += 1;
    }
    Ok(Some((index, argument)))
}
fn exact(value: String) -> String {
    value.into_boxed_str().into_string()
}
fn reserve<T>(values: &mut Vec<T>, capacity: usize) -> Result<(), LocalizationError> {
    if capacity > values.capacity() {
        values
            .try_reserve_exact(capacity - values.len())
            .map_err(|_| invalid("localization_limit", "catalog allocation"))?;
    }
    Ok(())
}
struct Budget(usize);
impl Budget {
    fn charge(&mut self, bytes: usize) -> Result<(), LocalizationError> {
        self.0 = self
            .0
            .checked_add(bytes)
            .filter(|value| *value <= MAX_RETAINED_BYTES)
            .ok_or_else(|| invalid("localization_limit", "retained catalog exceeds 8 MiB"))?;
        Ok(())
    }
    fn release(&mut self, bytes: usize) {
        self.0 -= bytes;
    }
}
fn valid_code(code: &str) -> bool {
    if code.len() > 35 {
        return false;
    }
    let mut segments = code.split('-');
    let first = segments.next().unwrap_or("");
    (2..=8).contains(&first.len())
        && first.bytes().all(|value| value.is_ascii_alphabetic())
        && segments.all(|segment| {
            !segment.is_empty()
                && segment.len() <= 8
                && segment.bytes().all(|value| value.is_ascii_alphanumeric())
        })
}
fn alias_priority(key: &str) -> u8 {
    if key.starts_with("Module.") {
        2
    } else if key.starts_with("SkyLive.") {
        1
    } else {
        0
    }
}
fn alias_text(text: &str) -> Cow<'_, str> {
    let value = qt_text(text);
    let end = without_ellipsis(&value).len();
    match value {
        Cow::Borrowed(value) => Cow::Borrowed(&value[..end]),
        Cow::Owned(mut value) => {
            value.truncate(end);
            Cow::Owned(value)
        }
    }
}
fn without_ellipsis(value: &str) -> &str {
    value
        .strip_suffix('…')
        .or_else(|| {
            value
                .strip_suffix("...")
                .filter(|prefix| !prefix.ends_with('.'))
        })
        .unwrap_or(value)
}
fn rust_fields(text: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut position = 0;
    while position < text.len() {
        let tail = &text[position..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            position += 2;
            continue;
        }
        if tail.starts_with('{') {
            if let Some(end) = tail.find('}') {
                let token = &tail[..=end];
                let name = tail[1..end].split(':').next().unwrap_or("");
                if name
                    .bytes()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == b'_')
                {
                    fields.push(token);
                }
                position += end + 1;
                continue;
            }
        }
        position += tail.chars().next().map_or(1, char::len_utf8);
    }
    fields.sort_unstable();
    fields
}

fn qt_text(text: &str) -> Cow<'_, str> {
    // Qt translations sometimes append a Latin mnemonic as "(&S)". Egui has
    // no such accelerator, so remove this hint rather than displaying a false one.
    if let Some(begin) = text.rfind("(&") {
        let tail = &text[begin + 2..];
        if let Some(letter) = tail.strip_suffix(')') {
            if letter.chars().count() == 1 && letter.chars().all(char::is_alphanumeric) {
                return qt_text(text[..begin].trim_end()).into_owned().into();
            }
        }
    }
    if !text.contains('&') {
        return Cow::Borrowed(text);
    }
    let mut output = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((index, value)) = chars.next() {
        if value != '&' {
            output.push(value);
            continue;
        }
        if chars.peek().is_some_and(|(_, next)| *next == '&') {
            chars.next();
            output.push('&');
            continue;
        }
        // Retain HTML entities and ordinary "A & B" punctuation. Qt mnemonic
        // markers precede a non-whitespace character, usually a letter/digit.
        let tail = &text[index + 1..];
        let entity = tail.find(';').is_some_and(|end| {
            end <= 12
                && tail[..end]
                    .chars()
                    .all(|value| value.is_ascii_alphanumeric() || value == '#')
        });
        if entity || !chars.peek().is_some_and(|(_, next)| next.is_alphanumeric()) {
            output.push('&');
        }
    }
    if output == text {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(output)
    }
}

struct Csv<'a> {
    bytes: &'a [u8],
    position: usize,
    row_number: usize,
}
impl<'a> Csv<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            bytes: text.as_bytes(),
            position: 0,
            row_number: 0,
        }
    }
    fn row(&mut self) -> Result<Option<Vec<String>>, LocalizationError> {
        if self.position == self.bytes.len() {
            return Ok(None);
        }
        self.row_number += 1;
        let mut cells = Vec::new();
        reserve(&mut cells, MAX_LANGUAGES + 1)?;
        loop {
            if cells.len() > MAX_LANGUAGES {
                return Err(invalid(
                    "localization_limit",
                    format!("row {} exceeds 129 columns", self.row_number),
                ));
            }
            cells.push(self.cell(cells.len() + 1)?);
            match self.bytes.get(self.position).copied() {
                Some(b',') => self.position += 1,
                Some(b'\r') => {
                    self.position += 1;
                    if self.bytes.get(self.position) == Some(&b'\n') {
                        self.position += 1;
                    }
                    break;
                }
                Some(b'\n') => {
                    self.position += 1;
                    break;
                }
                None => break,
                _ => {
                    return Err(invalid(
                        "localization_csv",
                        format!(
                            "unexpected data after quoted field in row {}",
                            self.row_number
                        ),
                    ));
                }
            }
        }
        Ok(Some(cells))
    }
    fn cell(&mut self, column: usize) -> Result<String, LocalizationError> {
        let mut field = Vec::new();
        let quoted = self.bytes.get(self.position) == Some(&b'"');
        if quoted {
            self.position += 1;
        }
        let mut closed = !quoted;
        while let Some(value) = self.bytes.get(self.position).copied() {
            if quoted {
                if value == b'"' {
                    self.position += 1;
                    if self.bytes.get(self.position) == Some(&b'"') {
                        self.position += 1;
                    } else {
                        closed = true;
                        break;
                    }
                } else {
                    self.position += 1;
                }
            } else {
                if matches!(value, b',' | b'\r' | b'\n') {
                    break;
                }
                if value == b'"' {
                    return Err(invalid(
                        "localization_csv",
                        format!(
                            "quote inside unquoted cell at row {}, column {column}",
                            self.row_number
                        ),
                    ));
                }
                self.position += 1;
            }
            if field.len() >= MAX_CELL_BYTES {
                return Err(invalid(
                    "localization_limit",
                    format!(
                        "cell exceeds 32768 bytes at row {}, column {column}",
                        self.row_number
                    ),
                ));
            }
            field.push(value);
        }
        if !closed {
            return Err(invalid(
                "localization_csv",
                format!(
                    "unterminated quote at row {}, column {column}",
                    self.row_number
                ),
            ));
        }
        String::from_utf8(field.into_boxed_slice().into_vec())
            .map_err(|cause| invalid("localization_utf8", cause.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    #[test]
    fn generated_original_aliases_match_rust_normalization_and_are_sorted_unique() {
        assert!(ORIGINAL_ALIASES.len() > 2000);
        for &(normalized, _, original) in ORIGINAL_ALIASES {
            assert_eq!(alias_text(original), normalized, "{original:?}");
        }
        for pair in ORIGINAL_ALIASES.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "aliases are strictly sorted and unique"
            );
        }
        for (source, expected) in [
            ("&Save...", "Save"),
            ("Save…", "Save"),
            ("Wait....", "Wait...."),
            ("A & B", "A & B"),
            ("A&B", "AB"),
            ("R&&D", "R&D"),
            ("Text &amp; HTML", "Text &amp; HTML"),
            ("Настройки (&S)", "Настройки"),
            ("Label (&Ж)", "Label"),
            ("Label (&SS)", "Label (SS)"),
            ("Keep whitespace ", "Keep whitespace "),
        ] {
            assert_eq!(alias_text(source), expected);
        }
    }

    #[test]
    fn edited_english_cells_keep_original_source_identity_and_drive_missing_translations() {
        let csv = concat!(
            "key,en-US,ru-RU\n",
            "Save,&Keep changes...,\n",
            "Apply,Confirm these settings,Подтвердить\n",
            "Basic.MainMenu.File.Settings,Preferences (&S),\n"
        );
        let english = Localization::from_csv(csv.as_bytes(), "en-US").unwrap();
        let russian = Localization::from_csv(csv.as_bytes(), "ru-RU").unwrap();
        for catalog in [&english, &russian] {
            assert_eq!(catalog.text("Save"), "Keep changes...");
            assert_eq!(catalog.text("Save…"), "Keep changes...");
            assert_eq!(catalog.text("&Save..."), "Keep changes...");
            assert_eq!(catalog.text("Settings"), "Preferences");
        }
        assert_eq!(english.text("Apply"), "Confirm these settings");
        assert_eq!(russian.text("Apply"), "Подтвердить");
        let unchanged = Localization::from_csv(b"key,en-US\nSave,Save\n", "en-US").unwrap();
        assert_eq!(
            unchanged.text("Save…"),
            "Save…",
            "unedited English preserves exact widget punctuation"
        );
        let sparse = Localization::from_csv(
            b"key,en-US,ru-RU\nCustom,&Save...,Custom translation\n",
            "ru-RU",
        )
        .unwrap();
        assert_eq!(
            sparse.text("Save"),
            "Custom translation",
            "absent stable key retains dynamic alias fallback"
        );
        assert!(Localization::from_csv(b"key,en-US\nSave,\"broken", "en-US").is_err());
        assert_eq!(
            english.text("Save"),
            "Keep changes...",
            "invalid replacement cannot mutate current catalog"
        );
    }

    #[test]
    fn qt_accelerator_suffix_and_edited_rust_fields_are_validated() {
        assert_eq!(qt_text("Настройки (&S)"), "Настройки");
        assert_eq!(qt_text("Research && Development"), "Research & Development");
        assert_eq!(qt_text("A & B"), "A & B");
        let valid = "key,en-US,ru-RU\nSkyLive.Format,Revision {value},Версия {value}\n";
        assert!(Localization::from_csv(valid.as_bytes(), "ru-RU").is_ok());
        for candidate in [
            valid.replace("Версия {value}", "Версия"),
            valid.replace("Версия {value}", "Версия {other}"),
        ] {
            assert_eq!(
                Localization::from_csv(candidate.as_bytes(), "ru-RU")
                    .unwrap_err()
                    .code(),
                "localization_format"
            );
        }
    }

    #[test]
    fn csv_quotes_commas_newlines_bom_and_empty_translations_have_literal_oracles() {
        let csv = "\u{feff}key,en-US,ru-RU\r\nSkyLive.LanguageName,English,Русский\r\n\"Key,comma\",\"Hello, \"\"world\"\"\nnext\",\"Привет, \"\"мир\"\"\r\nстрока\"\r\nFallback,&Fallback...,\r\nTrailing,Last,\r\n";
        let ru = Localization::from_csv(csv.as_bytes(), "ru-RU").unwrap();
        assert_eq!(ru.translate("Key,comma"), "Привет, \"мир\"\r\nстрока");
        assert_eq!(ru.translate("Fallback"), "&Fallback...");
        assert_eq!(ru.translate("not present"), "not present");
        assert_eq!(ru.missing(), 2);
        assert_eq!(
            ru.languages(),
            [
                Language {
                    code: "en-US".into(),
                    name: "English".into()
                },
                Language {
                    code: "ru-RU".into(),
                    name: "Русский".into()
                }
            ]
        );
        assert_eq!(
            ru.text("Fallback…"),
            "Fallback…",
            "missing translation preserves the original widget literal"
        );
        let english = Localization::from_csv(csv.as_bytes(), "en-US").unwrap();
        assert_eq!(english.translate("Key,comma"), "Hello, \"world\"\nnext");
        assert_eq!(english.text("&Fallback…"), "&Fallback…");
        assert_eq!(
            Localization::from_csv(b"key,en-US,ru-RU\nA,English,", "ru-RU")
                .unwrap()
                .translate("A"),
            "English"
        );
    }

    #[test]
    fn aliases_choose_frontend_then_app_then_module_and_strip_only_qt_display_markup() {
        let csv = concat!(
            "key,en-US,ru-RU\n",
            "Module.test.Open,&Open...,Модуль\n",
            "SkyLive.Open,Open…,Приложение\n",
            "Z.Frontend,Open,Поздний\n",
            "A.Frontend,&Open..., &Открыть...\n",
            "Ampersand,R&&D,Исследования && разработка\n",
            "Ordinary,Audio & Video,Аудио & Видео\n",
            "Entity,Text &amp; HTML,Текст &amp; HTML\n",
            "Dots,Version 1.2.3,Версия 1.2.3\n",
            "FourDots,Wait....,Ждать....\n"
        );
        let catalog = Localization::from_csv(csv.as_bytes(), "ru-RU").unwrap();
        assert_eq!(catalog.ambiguous_aliases(), 1);
        assert_eq!(catalog.text("Open…"), " Открыть...");
        assert_eq!(catalog.text("&Open..."), " Открыть...");
        assert_eq!(catalog.text("R&D"), "Исследования & разработка");
        assert_eq!(catalog.text("Audio & Video"), "Аудио & Видео");
        assert_eq!(catalog.text("Text &amp; HTML"), "Текст &amp; HTML");
        assert_eq!(catalog.text("Version 1.2.3"), "Версия 1.2.3");
        assert_eq!(catalog.text("Wait...."), "Ждать....");
        assert_eq!(
            catalog.text("Wait"),
            "Wait",
            "do not strip arbitrary punctuation or collapse four dots"
        );
        assert_eq!(
            catalog.text(" open "),
            " open ",
            "do not lowercase or trim semantic whitespace"
        );
        assert_eq!(
            catalog.translate("A.Frontend"),
            " &Открыть...",
            "stable-key lookup preserves original table values"
        );
    }

    #[test]
    fn malformed_candidate_headers_keys_shapes_quotes_utf8_and_locales_reject() {
        for (csv, code) in [
            ("", "localization_header"),
            ("wrong,en-US\nA,B", "localization_header"),
            ("key,ru-RU\nA,B", "localization_header"),
            ("key,en-US,en-US\nA,B,C", "localization_header"),
            ("key,en-US,key\nA,B,C", "localization_header"),
            ("key,en-US,not a locale\nA,B,C", "localization_header"),
            ("key,en-US\nA,B\nA,C", "localization_key"),
            ("key,en-US\n,Empty key", "localization_key"),
            ("key,en-US\nA,B,C", "localization_columns"),
            ("key,en-US\nA", "localization_columns"),
            ("key,en-US\n\n", "localization_columns"),
            ("key,en-US\nA,unquoted\"quote", "localization_csv"),
            ("key,en-US\nA,\"closed\"garbage", "localization_csv"),
            ("key,en-US\nA,\"never closed", "localization_csv"),
        ] {
            assert_eq!(
                Localization::from_csv(csv.as_bytes(), "en-US")
                    .unwrap_err()
                    .code(),
                code,
                "{csv:?}"
            );
        }
        assert_eq!(
            Localization::from_csv(&[0xff], "en-US").unwrap_err().code(),
            "localization_utf8"
        );
        for language in ["de-DE", "", "en_US", "../ru-RU"] {
            assert_eq!(
                Localization::from_csv(b"key,en-US\nA,B", language)
                    .unwrap_err()
                    .code(),
                "localization_language"
            );
        }
        let current = Localization::from_csv(b"key,en-US\nA,Valid", "en-US").unwrap();
        assert!(Localization::from_csv(b"key,en-US\nA,\"bad", "en-US").is_err());
        assert_eq!(
            current.translate("A"),
            "Valid",
            "a rejected replacement has no mutation authority"
        );
    }

    #[test]
    fn positional_formats_are_atomic_bounded_and_validate_english_argument_contract() {
        let csv = "key,en-US,ru-RU\nOrder,%1/%10/%2 100%,Десятый %10 / %1 / %2\nOmitted,Source %1,Перевод\nRepeat,%1%1,%1%1\nZero,%0,%0\nOverflow,%9999999999999999999999999999999999999999,x";
        let catalog = Localization::from_csv(csv.as_bytes(), "ru-RU").unwrap();
        let args = [
            "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        ];
        assert_eq!(
            catalog.format("Order", &args).unwrap(),
            "Десятый ten / one / two"
        );
        let injected = [
            "%2", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        ];
        assert_eq!(
            catalog.format("Order", &injected).unwrap(),
            "Десятый ten / %2 / two"
        );
        for (key, args) in [
            ("Order", &args[..9]),
            ("Omitted", &[][..]),
            ("Zero", &args[..]),
            ("Overflow", &args[..]),
        ] {
            assert_eq!(
                catalog.format(key, args).unwrap_err().code(),
                "localization_placeholder"
            );
        }
        let exact = "x".repeat(MAX_FORMAT_BYTES / 2);
        assert_eq!(
            catalog.format("Repeat", &[&exact]).unwrap().len(),
            MAX_FORMAT_BYTES
        );
        let too_large = "x".repeat(MAX_FORMAT_BYTES / 2 + 1);
        assert_eq!(
            catalog.format("Repeat", &[&too_large]).unwrap_err().code(),
            "localization_limit"
        );
        assert_eq!(catalog.format("100% literal", &[]).unwrap(), "100% literal");
    }

    #[test]
    fn file_row_cell_language_and_retained_byte_limits_are_independent() {
        let too_large = vec![b'x'; MAX_LOCALIZATION_FILE_BYTES + 1];
        assert_eq!(
            Localization::from_csv(&too_large, "en-US")
                .unwrap_err()
                .code(),
            "localization_limit"
        );
        drop(too_large);
        let exact = "😀".repeat(MAX_CELL_BYTES / 4);
        let csv = format!("key,en-US\nA,{exact}");
        assert_eq!(
            Localization::from_csv(csv.as_bytes(), "en-US")
                .unwrap()
                .translate("A")
                .len(),
            MAX_CELL_BYTES
        );
        let csv = format!("key,en-US\nA,{exact}x");
        assert_eq!(
            Localization::from_csv(csv.as_bytes(), "en-US")
                .unwrap_err()
                .code(),
            "localization_limit"
        );
        let mut rows = String::from("key,en-US\n");
        for index in 0..=MAX_ROWS {
            rows.push_str(&format!("Key{index},x\n"));
        }
        assert_eq!(
            Localization::from_csv(rows.as_bytes(), "en-US")
                .unwrap_err()
                .code(),
            "localization_limit"
        );
        let header = format!(
            "key,en-US,{}",
            (0..128)
                .map(|index| format!("qaa-{index:03}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(
            Localization::from_csv(header.as_bytes(), "en-US")
                .unwrap_err()
                .code(),
            "localization_limit"
        );
        // Only ~4 MiB of encoded input: retained aliases, English, selected
        // values and metadata independently exceed the 8 MiB read-model budget.
        let mut retained = String::from("key,en-US,ru-RU\n");
        let value = "x".repeat(MAX_CELL_BYTES);
        for index in 0..129 {
            retained.push_str(&format!("Key{index},{value},я\n"));
        }
        assert_eq!(
            Localization::from_csv(retained.as_bytes(), "ru-RU")
                .unwrap_err()
                .code(),
            "localization_limit"
        );
    }

    #[test]
    fn read_model_keeps_only_selected_english_and_names_and_io_cause_is_preserved() {
        let unselected = "x".repeat(MAX_CELL_BYTES);
        let csv = format!("key,en-US,ru-RU,de-DE\nA,English,Русский,{unselected}\n");
        let catalog = Localization::from_csv(csv.as_bytes(), "ru-RU").unwrap();
        assert_eq!(catalog.translate("A"), "Русский");
        assert!(
            catalog.retained_bytes() < 2048,
            "unselected language values are not retained"
        );
        static SERIAL: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!(
            "skylive-localization-{}-{}.csv",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, csv.as_bytes()).unwrap();
        let loaded = Localization::load(&path, "ru-RU").unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(loaded.translate("A"), "Русский");
        let error = Localization::load(&path, "ru-RU").unwrap_err();
        assert_eq!(error.code(), "localization_io");
        assert!(error.source().is_some());
    }

    #[test]
    fn actual_embedded_obs_keys_and_native_language_names_cover_five_acceptance_languages() {
        for (code, name, streaming, recording) in [
            ("en-US", "English", "Start Streaming", "Start Recording"),
            (
                "es-ES",
                "Español",
                "Iniciar Transmisión",
                "Iniciar Grabación",
            ),
            ("ru-RU", "Русский", "Начать трансляцию", "Начать запись"),
            (
                "uk-UA",
                "Українська",
                "Почати трансляцію",
                "Почати записування",
            ),
            ("de-DE", "Deutsch", "Stream starten", "Aufnahme starten"),
        ] {
            let catalog = Localization::embedded(code).unwrap();
            assert_eq!(catalog.language(), code);
            assert!(catalog.languages().len() >= 77);
            assert!(catalog.key_count() >= 2000);
            assert_eq!(
                catalog
                    .languages()
                    .iter()
                    .find(|language| language.code == code)
                    .unwrap()
                    .name,
                name
            );
            assert_eq!(catalog.translate("Basic.Main.StartStreaming"), streaming);
            assert_eq!(catalog.translate("Basic.Main.StartRecording"), recording);
            assert_eq!(catalog.text("Start Streaming"), streaming);
            assert!(
                catalog
                    .format("Undo.SceneCollection.Switch", &["Collection 7"])
                    .unwrap()
                    .contains("Collection 7")
            );
            assert!(catalog.missing() <= catalog.key_count());
            assert!(catalog.retained_bytes() <= MAX_RETAINED_BYTES);
        }
    }
}
