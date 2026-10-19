//! Hunspell dictionaries installed on the system, read at runtime.
//!
//! WordCraft doesn't bundle large spelling dictionaries. For languages with an affix dictionary
//! (Polish, Russian), it reads the standard Hunspell `.aff`/`.dic` pair installed by the operating system,
//! a package manager or LibreOffice, with [`crate::affix::AffixDict::from_hunspell`]. When none is
//! installed the language has no spelling check (no marks, no errors), like Word without the
//! language's proofing tools.
//!
//! Search order: the directory in `WORDCRAFT_DICTIONARIES` (or one set with
//! [`set_dictionary_dir`]), then the usual system locations. On the web there are none.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::affix::AffixDict;

/// Largest `.aff` or `.dic` file read (real ones are a few MB).
const MAX_FILE: u64 = 64 << 20;

/// An extra directory searched first (a setting), set by the front end.
static EXTRA_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Why an installed dictionary failed to load, by `.dic` name (`pt_BR`): the file and the reason.
/// Without it a file that is there but unreadable looks the same as no file (#2830).
static LOAD_ERRORS: RwLock<Vec<(String, PathBuf, String)>> = RwLock::new(Vec::new());

/// Remember (or, for `Ok`, forget) why the dictionary `key` found at `dic` failed to load, and
/// hand the result back unchanged.
pub fn note_load<T>(key: &str, dic: &Path, result: Result<T, String>) -> Result<T, String> {
    let mut errors = LOAD_ERRORS.write().unwrap_or_else(|e| e.into_inner());
    errors.retain(|(k, _, _)| k != key);
    if let Err(e) = &result {
        errors.push((key.to_string(), dic.to_path_buf(), e.clone()));
    }
    result
}

/// Forget the load error of the dictionary `key` (none found, or it is about to be searched for again).
pub fn clear_load_error(key: &str) {
    LOAD_ERRORS.write().unwrap_or_else(|e| e.into_inner()).retain(|(k, _, _)| k != key);
}

/// The `.dic` file and the reason of the last failed load of the dictionary `key`, if any.
pub fn load_error(key: &str) -> Option<(PathBuf, String)> {
    LOAD_ERRORS.read().unwrap_or_else(|e| e.into_inner()).iter().find(|(k, _, _)| k == key).map(|(_, p, e)| (p.clone(), e.clone()))
}

/// Search `dir` before the system locations (a "dictionaries folder" setting); `None` clears it.
/// Takes effect for dictionaries not loaded yet.
pub fn set_dictionary_dir(dir: Option<PathBuf>) {
    *EXTRA_DIR.write().unwrap_or_else(|e| e.into_inner()) = dir;
    LOAD_ERRORS.write().unwrap_or_else(|e| e.into_inner()).clear(); // they belong to the old search path
}

/// The LibreOffice extension directories a Hunspell dictionary name may live in: LibreOffice's
/// IDs are per dictionary — Polish is `dict-pl`, Brazilian Portuguese `dict-pt-BR` — so both
/// the primary-language folder and the dashed full name are tried.
fn lo_ext_dirs(name: &str) -> Vec<String> {
    let primary = name.split(['-', '_']).next().unwrap_or(name);
    let dashed = name.replace('_', "-");
    let first = format!("dict-{primary}");
    let full = format!("dict-{dashed}");
    if full == first { vec![first] } else { vec![first, full] }
}

/// Directories that may hold Hunspell dictionaries, most specific first. `lo_exts`, when a
/// language is being searched for, adds LibreOffice's extension folders for it (`dict-pt-BR`).
pub fn search_dirs(lo_exts: Option<&[String]>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = EXTRA_DIR.read().unwrap_or_else(|e| e.into_inner()).clone() {
        dirs.push(d);
    }
    if cfg!(target_arch = "wasm32") {
        return dirs;
    }
    if let Some(d) = std::env::var_os("WORDCRAFT_DICTIONARIES").filter(|v| !v.is_empty()) {
        dirs.push(PathBuf::from(d));
    }
    // LibreOffice keeps each language under `…/share/extensions/<dict-xx>`.
    let lo_dirs =
        |prefix: &str| -> Vec<PathBuf> { lo_exts.map_or_else(Vec::new, |exts| exts.iter().map(|e| PathBuf::from(prefix).join(e)).collect()) };
    let home = std::env::var_os("HOME").filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        for var in ["LOCALAPPDATA", "APPDATA"] {
            if let Some(base) = std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from) {
                dirs.push(base.join("WordCraft").join("dictionaries"));
                dirs.push(base.join("hunspell"));
            }
        }
        for var in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(base) = std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from) {
                dirs.extend(lo_dirs(&base.join("LibreOffice").join("share").join("extensions").to_string_lossy()));
            }
        }
        // If LibreOffice is installed on another drive (e.g. D:\Program Files\LibreOffice), check drives C..Z
        for drive in b'C'..=b'Z' {
            let drive_char = drive as char;
            for pf in ["Program Files", "Program Files (x86)"] {
                let lo_base = PathBuf::from(format!(r"{drive_char}:\{pf}\LibreOffice\share\extensions"));
                if lo_base.is_dir() {
                    dirs.extend(lo_dirs(&lo_base.to_string_lossy()));
                }
            }
        }
    } else if cfg!(target_os = "macos") {
        if let Some(h) = &home {
            dirs.push(h.join("Library/Spelling"));
        }
        dirs.push(PathBuf::from("/Library/Spelling"));
        dirs.push(PathBuf::from("/opt/homebrew/share/hunspell"));
        dirs.push(PathBuf::from("/usr/local/share/hunspell"));
        dirs.extend(lo_dirs("/Applications/LibreOffice.app/Contents/Resources/extensions"));
    } else {
        if let Some(h) = &home {
            dirs.push(h.join(".local/share/hunspell"));
        }
        for d in ["/usr/share/hunspell", "/usr/share/myspell", "/usr/share/myspell/dicts", "/usr/local/share/hunspell", "/usr/local/share/myspell"] {
            dirs.push(PathBuf::from(d));
        }
        for d in ["/usr/lib/libreoffice/share/extensions", "/opt/libreoffice/share/extensions"] {
            dirs.extend(lo_dirs(d));
        }
    }
    dirs
}

/// The first `<name>.aff`/`<name>.dic` pair found for any of `names` (`pl_PL`, `pl`), searching
/// [`search_dirs`] (including LibreOffice's extension folder for the first name).
pub fn find(names: &[&str]) -> Option<(PathBuf, PathBuf)> {
    let lo = names.first().copied().map(lo_ext_dirs);
    search_dirs(lo.as_deref()).into_iter().find_map(|dir| find_in(&dir, names))
}

/// The first `<name>.aff`/`<name>.dic` pair for any of `names` in `dir`.
pub fn find_in(dir: &Path, names: &[&str]) -> Option<(PathBuf, PathBuf)> {
    names.iter().find_map(|n| {
        let aff = dir.join(format!("{n}.aff"));
        let dic = dir.join(format!("{n}.dic"));
        (aff.is_file() && dic.is_file()).then_some((aff, dic))
    })
}

/// Read a Hunspell pair from disk (size-capped), decoded per the `.aff`'s `SET` line.
pub fn load(aff: &Path, dic: &Path) -> Result<AffixDict, String> {
    let read = |p: &Path| -> Result<Vec<u8>, String> {
        let len = std::fs::metadata(p).map_err(|e| format!("{}: {e}", p.display()))?.len();
        if len > MAX_FILE {
            return Err(format!("{}: too large ({len} bytes)", p.display()));
        }
        std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))
    };
    let (aff_text, dic_text) = decode(&read(aff)?, &read(dic)?)?;
    let source = format!("Hunspell dictionary {}", dic.display());
    AffixDict::from_hunspell(&aff_text, &dic_text, &[source.as_str()])
}

/// ISO 8859-1: every byte is the code point of the same value (the original VERO pt_BR uses it).
fn decode_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// ISO 8859-2 code points 0xA0..=0xFF (0x00..=0x9F map to themselves).
const ISO_8859_2_HIGH: &str = "\u{a0}Ą˘Ł¤ĽŚ§¨ŠŞŤŹ\u{ad}ŽŻ°ą˛ł´ľśˇ¸šşťź˝žżŔÁÂĂÄĹĆÇČÉĘËĚÍÎĎĐŃŇÓÔŐÖ×ŘŮÚŰÜÝŢßŕáâăäĺćçčéęëěíîďđńňóôőö÷řůúűüýţ˙";

fn decode_latin2(bytes: &[u8]) -> String {
    let high: Vec<char> = ISO_8859_2_HIGH.chars().collect();
    bytes.iter().map(|&b| if b < 0xa0 { char::from(b) } else { high.get(usize::from(b - 0xa0)).copied().unwrap_or('\u{fffd}') }).collect()
}

/// KOI8-R code points 0x80..=0xFF (0x00..=0x7F are ASCII): the encoding of older Russian
/// dictionaries.
const KOI8_R_HIGH: &str =
    "─│┌┐└┘├┤┬┴┼▀▄█▌▐░▒▓⌠■∙√≈≤≥\u{a0}⌡°²·÷═║╒ё╓╔╕╖╗╘╙╚╛╜╝╞╟╠╡Ё╢╣╤╥╦╧╨╩╪╫╬©юабцдефгхийклмнопярстужвьызшэщчъЮАБЦДЕФГХИЙКЛМНОПЯРСТУЖВЬЫЗШЭЩЧЪ";

/// Windows-1251 code points 0x80..=0xFF (0x98 is unassigned): the other one Russian dictionaries
/// use.
const CP1251_HIGH: &str = "ЂЃ‚ѓ„…†‡€‰Љ‹ЊЌЋЏђ‘’“”•–—\u{fffd}™љ›њќћџ\u{a0}ЎўЈ¤Ґ¦§Ё©Є«¬\u{ad}®Ї°±Ііґµ¶·ё№є»јЅѕїАБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдежзийклмнопрстуфхцчшщъыьэюя";

/// Decode a single-byte encoding whose bytes 0x80..=0xFF are `high` (ASCII below).
fn decode_high(bytes: &[u8], high: &str) -> String {
    let high: Vec<char> = high.chars().collect();
    bytes.iter().map(|&b| if b < 0x80 { char::from(b) } else { high.get(usize::from(b - 0x80)).copied().unwrap_or('\u{fffd}') }).collect()
}

/// Decode an `.aff`/`.dic` pair to UTF-8 from the encoding the `.aff`'s `SET` line names
/// (UTF-8 when there is none; ISO 8859-2 is the other one Polish dictionaries use, ISO 8859-1 the
/// one the original Portuguese VERO uses, KOI8-R and Windows-1251 the ones older Russian dictionaries use).
///
/// Under a UTF-8 `SET` without `FLAG UTF-8`, flags are still single bytes, as in Hunspell: the
/// Hungarian dictionary writes its affix flags and flag aliases as bytes above 0x7F that aren't
/// UTF-8. Those fields are read byte by byte (each byte becomes U+0000..U+00FF, one flag per
/// byte); the words, affixes and conditions around them are UTF-8.
pub fn decode(aff: &[u8], dic: &[u8]) -> Result<(String, String), String> {
    let head = String::from_utf8_lossy(aff.get(..aff.len().min(8192)).unwrap_or_default()).to_string();
    let directive = |name: &str| {
        head.lines().find_map(|l| {
            let l = l.trim_start_matches('\u{feff}');
            // Only the first argument counts, as in Hunspell: `FLAG UTF-8 # Unicode flags` is UTF-8.
            l.strip_prefix(name)
                .filter(|rest| rest.starts_with([' ', '\t']))
                .and_then(|rest| rest.split([' ', '\t']).find(|f| !f.is_empty()))
                .map(str::to_ascii_uppercase)
        })
    };
    let set = directive("SET").unwrap_or_else(|| "UTF-8".into());
    match set.as_str() {
        "UTF-8" | "UTF8" => {
            if matches!(directive("FLAG").as_deref(), Some("UTF-8" | "UTF8")) {
                let text = |b: &[u8], what: &str| String::from_utf8(b.to_vec()).map_err(|e| format!("{what}: not UTF-8: {e}"));
                return Ok((text(aff, ".aff")?, text(dic, ".dic")?));
            }
            Ok((decode_utf8_aff(aff)?, decode_utf8_dic(dic)?))
        }
        "ISO8859-1" | "ISO-8859-1" => Ok((decode_latin1(aff), decode_latin1(dic))),
        "ISO8859-2" | "ISO-8859-2" => Ok((decode_latin2(aff), decode_latin2(dic))),
        "KOI8-R" | "KOI8R" => Ok((decode_high(aff, KOI8_R_HIGH), decode_high(dic, KOI8_R_HIGH))),
        "CP1251" | "WINDOWS-1251" | "MICROSOFT-CP1251" => Ok((decode_high(aff, CP1251_HIGH), decode_high(dic, CP1251_HIGH))),
        other => Err(format!("unsupported encoding {other}")),
    }
}

/// Directives whose one argument is a flag.
const FLAG_DIRECTIVES: &[&str] = &[
    "CIRCUMFIX",
    "COMPOUNDBEGIN",
    "COMPOUNDEND",
    "COMPOUNDFLAG",
    "COMPOUNDFORBIDFLAG",
    "COMPOUNDLAST",
    "COMPOUNDFIRST",
    "COMPOUNDMIDDLE",
    "COMPOUNDPERMITFLAG",
    "COMPOUNDROOT",
    "FORBIDDENWORD",
    "FORCEUCASE",
    "GENERATE",
    "HU_KOTOHANGZO",
    "KEEPCASE",
    "LEMMA_PRESENT",
    "NEEDAFFIX",
    "NOSUGGEST",
    "ONLYINCOMPOUND",
    "ONLYROOT",
    "PSEUDOROOT",
    "SUBSTANDARD",
    "WARN",
];

/// Directives that only describe the dictionary; a non-UTF-8 byte in them (the Hungarian
/// dictionary's `NAME` is ISO 8859-2) is replaced instead of failing the load.
const METADATA_DIRECTIVES: &[&str] = &["NAME", "HOME", "VERSION", "LANG", "AM"];

/// One byte per char: a flag field read as single-byte flags.
fn bytes_as_flags(b: &[u8]) -> String {
    b.iter().map(|&x| char::from(x)).collect()
}

/// `text/flags`: the text as UTF-8 and the flags byte by byte (`\/` is a literal slash).
fn text_and_flags(field: &[u8], what: &str) -> Result<String, String> {
    let mut slash = None;
    let mut i = 0;
    while let Some(&b) = field.get(i) {
        if b == b'\\' && field.get(i + 1) == Some(&b'/') {
            i += 2;
            continue;
        }
        if b == b'/' {
            slash = Some(i);
            break;
        }
        i += 1;
    }
    let (text, flags) = match slash {
        Some(k) => (field.get(..k).unwrap_or_default(), field.get(k..).unwrap_or_default()),
        None => (field, &[][..]),
    };
    let mut out = std::str::from_utf8(text).map_err(|e| format!("{what}: not UTF-8: {e}"))?.to_string();
    out.push_str(&bytes_as_flags(flags));
    Ok(out)
}

/// Split a line into fields on spaces and tabs, keeping the separators (so a line rebuilt from
/// its parts is the original).
fn fields(line: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_sep = line.first().is_some_and(|b| matches!(b, b' ' | b'\t'));
    for (i, b) in line.iter().enumerate() {
        let sep = matches!(b, b' ' | b'\t');
        if sep != in_sep {
            out.push(line.get(start..i).unwrap_or_default());
            start = i;
            in_sep = sep;
        }
    }
    out.push(line.get(start..).unwrap_or_default());
    out
}

/// A UTF-8 `.aff` with single-byte flags (see [`decode`]).
fn decode_utf8_aff(aff: &[u8]) -> Result<String, String> {
    let mut out = String::with_capacity(aff.len());
    for (n, line) in aff.split(|&b| b == b'\n').enumerate() {
        if n > 0 {
            out.push('\n');
        }
        if let Ok(s) = std::str::from_utf8(line)
            && s.is_ascii()
        {
            out.push_str(s);
            continue;
        }
        let parts = fields(line);
        // Field index among the non-separator parts.
        let words: Vec<usize> =
            parts.iter().enumerate().filter(|(_, p)| !p.first().is_some_and(|b| matches!(b, b' ' | b'\t'))).map(|(i, _)| i).collect();
        let dir = words
            .first()
            .and_then(|&i| parts.get(i))
            .map(|p| String::from_utf8_lossy(p).trim_start_matches('\u{feff}').to_string())
            .unwrap_or_default();
        let what = format!(".aff line {}", n + 1);
        if dir.starts_with('#') || METADATA_DIRECTIVES.contains(&dir.as_str()) {
            out.push_str(&String::from_utf8_lossy(line));
            continue;
        }
        // A trailing `# comment` (the Hungarian `AF` lines number themselves) is never a word or
        // a flag; from there on the line is kept as text. It can only start after the directive's
        // own fields, since a field such as an affix condition may itself begin with `#`.
        let field_text = |k: usize| words.get(k).and_then(|&w| parts.get(w)).map(|p| String::from_utf8_lossy(p).into_owned());
        let first_free = comment_field(&dir, field_text);
        let comment_at = words.iter().skip(first_free).position(|&w| parts.get(w).is_some_and(|f| f.first() == Some(&b'#'))).map(|k| k + first_free);
        for (i, p) in parts.iter().enumerate() {
            let field = words.iter().position(|&w| w == i);
            let in_comment = comment_at.is_some_and(|c| words.get(c).is_some_and(|&w| i >= w));
            let decoded = match (dir.as_str(), field) {
                _ if in_comment => String::from_utf8_lossy(p).into_owned(),
                ("AF" | "COMPOUNDRULE" | "SYLLABLENUM", Some(1)) => bytes_as_flags(p),
                ("PFX" | "SFX", Some(3)) | ("CHECKCOMPOUNDPATTERN", Some(1 | 2)) => text_and_flags(p, &what)?,
                // A field that names one flag keeps its first byte, as Hunspell's `decode_flag`
                // does: `SFX é` is the flag 0xC3, the first of the two flags `kert/é` carries.
                ("PFX" | "SFX", Some(1)) => bytes_as_flags(p.get(..1).unwrap_or_default()),
                (d, Some(1)) if FLAG_DIRECTIVES.contains(&d) => bytes_as_flags(p.get(..1).unwrap_or_default()),
                _ => std::str::from_utf8(p).map_err(|e| format!("{what}: not UTF-8: {e}"))?.to_string(),
            };
            out.push_str(&decoded);
        }
    }
    Ok(out)
}

/// The first field of an `.aff` line where a trailing `# comment` may start: after the
/// directive's own fields. A table's count line (`REP 12`, `PFX A Y 3`) has fewer fields than its
/// rows (`REP a b`, `PFX A 0 x .`). `field(k)` is the line's `k`-th field.
fn comment_field(dir: &str, field: impl Fn(usize) -> Option<String>) -> usize {
    let number = |k: usize| field(k).is_some_and(|f| f.parse::<usize>().is_ok());
    match dir {
        "PFX" | "SFX" if field(2).is_some_and(|f| f == "Y" || f == "N") && number(3) => 4,
        "PFX" | "SFX" => 5,
        "REP" | "ICONV" | "OCONV" | "PHONE" | "MAP" | "BREAK" | "AF" | "AM" | "COMPOUNDRULE" | "CHECKCOMPOUNDPATTERN"
            if number(1) && field(2).is_none_or(|f| f.starts_with('#')) =>
        {
            2
        }
        "REP" | "ICONV" | "OCONV" | "PHONE" => 3,
        "CHECKCOMPOUNDPATTERN" => 4,
        _ => 2,
    }
}

/// Where the `word/flags` entry of a `.dic` line ends, as Hunspell reads it: at a tab or at the
/// first morphological field (a space or tab, two characters and `:`, as in ` po:noun`), whichever
/// comes first, so an entry may hold spaces (`ad hoc`); with neither, at the first space.
pub(crate) fn dic_entry_end(line: &[u8]) -> usize {
    let blank = |b: &u8| matches!(b, b' ' | b'\t');
    let morph = line.windows(4).position(|w| matches!(w, [a, b, c, b':'] if blank(a) && !blank(b) && !blank(c)));
    let tab = line.iter().position(|&b| b == b'\t');
    let end = match (morph, tab) {
        (Some(m), Some(t)) => m.min(t),
        (m, t) => match m.or(t) {
            Some(e) => e,
            None => line.iter().position(|&b| b == b' ').unwrap_or(line.len()),
        },
    };
    // Spaces before the morphological field aren't part of the entry.
    line.get(..end).map_or(end, |e| e.iter().rposition(|b| !blank(b)).map_or(0, |k| k + 1))
}

/// A UTF-8 `.dic` with single-byte flags: the word is UTF-8, the flags after `/` are bytes.
fn decode_utf8_dic(dic: &[u8]) -> Result<String, String> {
    let mut out = String::with_capacity(dic.len());
    for (n, line) in dic.split(|&b| b == b'\n').enumerate() {
        if n > 0 {
            out.push('\n');
        }
        if let Ok(s) = std::str::from_utf8(line)
            && s.split_once('/').is_none_or(|(_, flags)| flags.is_ascii())
        {
            out.push_str(s);
            continue;
        }
        let what = format!(".dic line {}", n + 1);
        // The first field is `word/flags` (it may hold spaces, see [`dic_entry_end`]);
        // morphological fields after it are UTF-8 text.
        let end = dic_entry_end(line);
        out.push_str(&text_and_flags(line.get(..end).unwrap_or_default(), &what)?);
        let rest = line.get(end..).unwrap_or_default();
        out.push_str(std::str::from_utf8(rest).map_err(|e| format!("{what}: not UTF-8: {e}"))?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_latin2_and_utf8() {
        let aff = b"SET ISO8859-2\nTRY a\n";
        let (a, d) = decode(aff, b"2\n\xbf\xf3\xb3w\n").unwrap();
        assert!(a.contains("TRY a"));
        assert_eq!(d, "2\nżółw\n");
        let (_, d) = decode("SET UTF-8\n".as_bytes(), "żółw".as_bytes()).unwrap();
        assert_eq!(d, "żółw");
        assert!(decode(b"SET SHIFT_JIS\n", b"").is_err());
        assert!(decode(b"SET UTF-8\n", b"\xff\xfe").is_err());
    }

    /// #2830: a dictionary that is there but can't be read leaves its reason behind; a good
    /// load or no file clears it.
    #[test]
    fn load_error_is_remembered_until_the_next_good_load() {
        let key = "test_unreadable_2830";
        assert_eq!(load_error(key), None);
        let dic = Path::new("/dicts/test.dic");
        assert!(note_load(key, dic, decode(b"SET SHIFT_JIS\n", b"")).is_err());
        let (file, reason) = load_error(key).expect("error kept");
        assert_eq!(file, dic);
        assert_eq!(reason, "unsupported encoding SHIFT_JIS");
        assert!(note_load(key, dic, decode(b"SET UTF-8\n", b"")).is_ok());
        assert_eq!(load_error(key), None);
        let _ = note_load::<()>(key, dic, Err("too large".into()));
        clear_load_error(key);
        assert_eq!(load_error(key), None);
    }

    #[test]
    fn decodes_latin1() {
        // `orçamento esperávamos` in ISO 8859-1: ç = 0xE7, á = 0xE1.
        let (a, d) = decode(b"SET ISO8859-1\nTRY \xe7\n", b"1\nor\xe7amento/A\nesper\xe1vamos\n").unwrap();
        assert!(a.contains("TRY ç"), "{a}");
        assert_eq!(d, "1\norçamento/A\nesperávamos\n");
        assert!(decode(b"SET ISO-8859-1\n", b"\xe7").is_ok());
    }

    #[test]
    fn utf8_dictionaries_keep_single_byte_flags() {
        // The Hungarian dictionary is `SET UTF-8` but writes flags as single bytes above 0x7F,
        // which aren't UTF-8; each byte is one flag. Its `NAME` line is ISO 8859-2.
        let aff = b"SET UTF-8\nNAME Magyar helyes\xedr\xe1si\nAF 1\nAF \xd5a # 1\nSFX \xd5 Y 1\nSFX \xd5 0 ok/\xe9 . 7\nSFX \xe9 Y 1\nSFX \xe9 0 ban .\nSFX a Y 1\nSFX a 0 at .\n";
        let dic = "2\nház/1\t1\nkert/\u{e9}\n".as_bytes();
        let (a, d) = decode(aff, dic).unwrap();
        assert!(a.contains("AF \u{d5}a # 1") && a.contains("SFX \u{d5} 0 ok/\u{e9} . 7"));
        assert!(d.contains("kert/\u{c3}\u{a9}"), "a UTF-8 é in the flags is two one-byte flags");
        let dict = crate::affix::AffixDict::from_hunspell(&a, &d, &[]).unwrap();
        assert!(dict.check("ház") && dict.check("házok") && dict.check("házokban") && dict.check("házat"));
        assert!(!dict.check("házban"));
        // `FLAG UTF-8` keeps the whole file UTF-8, and words must still be UTF-8.
        let (_, d) = decode(b"SET UTF-8\nFLAG UTF-8\n", "kert/\u{e9}\n".as_bytes()).unwrap();
        assert_eq!(d, "kert/\u{e9}\n");
        assert!(decode(b"SET UTF-8\nSFX a Y 1\nSFX a 0 \xe9 .\n", b"").is_err(), "an affix must be UTF-8");
        assert!(decode(b"SET UTF-8\n", b"1\nh\xe1z/a\n").is_err(), "a stem must be UTF-8");
    }

    #[test]
    fn single_flag_fields_keep_their_first_byte() {
        // Without `FLAG UTF-8`, a UTF-8 `é` (0xC3 0xA9) in a field that names one flag is the
        // flag 0xC3; in a `.dic` flag list it is the two flags 0xC3 and 0xA9.
        let aff = "SET UTF-8\nKEEPCASE é\nSFX é Y 1\nSFX é 0 ok .\nSYLLABLENUM é\n".as_bytes();
        let (a, d) = decode(aff, "2\nkert/é\nNATO/é\n".as_bytes()).unwrap();
        assert!(a.contains("KEEPCASE \u{c3}\n") && a.contains("SFX \u{c3} Y 1") && a.contains("SFX \u{c3} 0 ok ."), "{a}");
        assert!(a.contains("SYLLABLENUM \u{c3}\u{a9}"), "SYLLABLENUM lists flags: {a}");
        assert!(d.contains("kert/\u{c3}\u{a9}"));
        let dict = crate::affix::AffixDict::from_hunspell(&a, &d, &[]).unwrap();
        assert!(dict.check("kert") && dict.check("kertok"));
        assert!(dict.check("NATO") && !dict.check("Nato"), "KEEPCASE é reaches NATO/é");
    }

    #[test]
    fn a_comment_after_flag_utf8_keeps_unicode_flags() {
        let (_, d) = decode(b"SET UTF-8\nFLAG UTF-8 # Unicode flags\n", "kert/\u{e9}\n".as_bytes()).unwrap();
        assert_eq!(d, "kert/\u{e9}\n");
        let (_, d) = decode(b"SET\tUTF-8  # comment\nFLAG\t\tUTF-8\n", "kert/\u{e9}\n".as_bytes()).unwrap();
        assert_eq!(d, "kert/\u{e9}\n", "repeated spaces and tabs separate the argument");
    }

    #[test]
    fn decodes_russian_single_byte_encodings() {
        // `ёлка` in KOI8-R (0xA3 0xCC 0xCB 0xC1) and in Windows-1251 (0xB8 0xEB 0xEA 0xE0).
        let (a, d) = decode(b"SET KOI8-R\nTRY \xcf\xc5\n", b"1\n\xa3\xcc\xcb\xc1/A\n").unwrap();
        assert!(a.contains("TRY ое"), "{a}");
        assert_eq!(d, "1\nёлка/A\n");
        let (_, d) = decode(b"SET microsoft-cp1251\n", b"1\n\xb8\xeb\xea\xe0 \xc4\xee\xec\n").unwrap();
        assert_eq!(d, "1\nёлка Дом\n");
        assert_eq!(decode_high(&[0x98], CP1251_HIGH), "\u{fffd}");
    }

    #[test]
    fn comments_start_after_the_directive_fields() {
        let f = |line: &'static str| move |k: usize| line.split_whitespace().nth(k).map(str::to_string);
        assert_eq!(comment_field("PFX", f("PFX A Y 3 # count")), 4);
        assert_eq!(comment_field("SFX", f("SFX A 0 # .")), 5, "a rule's condition may be `#`");
        assert_eq!(comment_field("AF", f("AF 1234 # aliases")), 2);
        assert_eq!(comment_field("AF", f("AF Aa # 1")), 2);
        assert_eq!(comment_field("REP", f("REP 12")), 2);
        assert_eq!(comment_field("REP", f("REP # x")), 3);
        assert_eq!(comment_field("CHECKCOMPOUNDPATTERN", f("CHECKCOMPOUNDPATTERN a b c")), 4);
        assert_eq!(comment_field("KEEPCASE", f("KEEPCASE k")), 2);
        // A byte flag after an append text that begins with `#` stays a flag, not a comment.
        let (a, _) = decode(b"SET UTF-8\nSFX \xd5 Y 1 # header\nSFX \xd5 0 #/\xe9 . # rule\n", b"").unwrap();
        assert!(a.contains("SFX \u{d5} 0 #/\u{e9} . # rule"), "{a}");
    }

    #[test]
    fn dic_entries_end_where_hunspell_ends_them() {
        let end = |l: &str| l.get(..dic_entry_end(l.as_bytes())).unwrap().to_string();
        assert_eq!(end("word/A po:noun\tts:x"), "word/A", "the morphological field comes before the tab");
        assert_eq!(end("ad hoc/B\tpo:adv"), "ad hoc/B", "a tab ends a phrase");
        assert_eq!(end("ad hoc/B po:adv"), "ad hoc/B");
        assert_eq!(end("word/A 123"), "word/A", "neither: the first space");
        assert_eq!(end("word"), "word");
        assert_eq!(end("a:b/A"), "a:b/A", "a colon inside the word isn't a field");
        // Single-byte flags of a phrase entry are read byte by byte, then the UTF-8 morphology.
        let (_, d) = decode(b"SET UTF-8\n", b"1\nad hoc/\xe9\tpo:adv\n").unwrap();
        assert_eq!(d, "1\nad hoc/\u{e9}\tpo:adv\n");
    }

    #[test]
    fn libreoffice_extension_dirs_follow_the_language() {
        // LibreOffice's IDs are per dictionary: Polish is `dict-pl`, Brazilian Portuguese
        // `dict-pt-BR`, so both the primary-language folder and the dashed full name are tried.
        assert_eq!(super::lo_ext_dirs("cs_CZ"), ["dict-cs", "dict-cs-CZ"]);
        assert_eq!(super::lo_ext_dirs("id_ID"), ["dict-id", "dict-id-ID"]);
        assert_eq!(super::lo_ext_dirs("pl_PL"), ["dict-pl", "dict-pl-PL"]);
        assert_eq!(super::lo_ext_dirs("pt_BR"), ["dict-pt", "dict-pt-BR"]);
        assert_eq!(super::lo_ext_dirs("de-DE"), ["dict-de", "dict-de-DE"]);
        let dirs = search_dirs(Some(&super::lo_ext_dirs("pt_BR")));
        assert!(dirs.iter().any(|d| d.ends_with("dict-pt-BR")), "{dirs:?}");
        assert!(search_dirs(None).iter().all(|d| !d.to_string_lossy().contains("dict-")), "no language: no LibreOffice guess");
    }

    #[test]
    fn finds_and_loads_a_pair_in_a_directory() {
        let dir = std::env::temp_dir().join(format!("wordcraft-hunspell-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(find_in(&dir, &["xx_XX"]).is_none(), "no files: nothing found");
        std::fs::write(dir.join("xx_XX.aff"), "SET UTF-8\nSFX A Y 1\nSFX A 0 y .\n").unwrap();
        assert!(find_in(&dir, &["xx_XX"]).is_none(), "an .aff alone isn't a dictionary");
        std::fs::write(dir.join("xx_XX.dic"), "1\nkot/A\n").unwrap();
        let (aff, dic) = find_in(&dir, &["yy", "xx_XX"]).unwrap();
        let d = load(&aff, &dic).unwrap();
        assert!(d.check("kot") && d.check("koty") && !d.check("kotx"));
        assert!(load(&dir.join("missing.aff"), &dic).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
