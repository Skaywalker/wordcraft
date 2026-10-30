//! Compound words and word breaks, as Hunspell's affix files describe them.
//!
//! Languages such as Hungarian write compounds as one word (`almafa`, `házkulcsban`), so a
//! dictionary lists the parts and marks which may join:
//!
//! - **Compound flags.** A part needs `COMPOUNDFLAG` or the flag for its place (`COMPOUNDBEGIN`,
//!   `COMPOUNDMIDDLE`, `COMPOUNDEND`), on the stem or on an affix's continuation classes. A
//!   prefix may start a compound and a suffix may end it; a prefix on the last part needs
//!   `COMPOUNDPERMITFLAG`, a suffix before the end that or a compound flag of its own.
//!   `COMPOUNDFORBIDFLAG`, on a stem or a suffix, keeps the word out of the first and middle
//!   places (`nárciszmetrózik` ends in one). `ONLYINCOMPOUND` stems and affixes appear only
//!   inside compounds; parts are at least `COMPOUNDMIN` letters (default 3), and a part before
//!   the last carries at most one suffix (two with `COMPOUNDMORESUFFIXES`). Homonyms (`szán` the
//!   noun and the verb) are separate entries, as in Hunspell.
//! - **Size.** At most `COMPOUNDWORDMAX` words (a `COMPOUNDROOT` stem, itself a compound, counts
//!   as two). Past that a compound is still correct when it has at most the `COMPOUNDSYLLABLE`
//!   count of syllables: Hungarian's 6–3 rule, where a compound of more than two words and more
//!   than six syllables takes a hyphen; in a Hungarian dictionary (`LANG hu_HU`) a prefix of two
//!   syllables or more on any part counts as a word. Syllables are the vowels
//!   `COMPOUNDSYLLABLE` lists; the
//!   inflections on the last part don't count. A suffix after which the word may compound again
//!   forms a new word and counts, as do the suffix classes `SYLLABLENUM` lists. An inflected
//!   form listed as a stem counts as its lemma (`st:` in its morphological fields: `falevelek`
//!   as `falevél`).
//! - **Boundaries.** `CHECKCOMPOUNDDUP` (no part twice in a row), `CHECKCOMPOUNDTRIPLE` (no
//!   letter three times across a boundary: `sakkkör`), `CHECKCOMPOUNDCASE` (no capital at a
//!   boundary), `CHECKCOMPOUNDPATTERN` (no part ending in one string, or with `0` a stem as
//!   listed, followed by one starting with another; a simplified spelling the pattern may name
//!   isn't accepted) and `CHECKCOMPOUNDREP` (no compound, and no two neighbouring parts, that is a
//!   `REP` misspelling of a word: `hajlak` for `haljak`). Two neighbouring parts that together
//!   are a listed word are read as that word (`szezon` + `légzáró`, never `lég` + `záró`).
//! - **`COMPOUNDRULE`**: patterns of flags with `*` and `?` (`0*1`: numbers such as
//!   `1999-ben`), matched by stems of at least `COMPOUNDMIN` letters, as many as the word holds,
//!   the last of which may carry suffixes.
//! - **`BREAK`**: strings at which a word is split and both sides checked like words, each in its
//!   own letter case (`Petőfi-vers`, `alma-Fa`; a word in capitals is split capitalized); `^`
//!   and `$` anchor one to the start or the end (`-féle$`). A word with ten break points or more
//!   is wrong, and a reading that is a listed `FORBIDDENWORD` isn't split, as in Hunspell; in a
//!   Hungarian dictionary the left side of a hyphen may be listed with it (`vegyiáru-`). Only
//!   dictionaries that declare
//!   `BREAK` get breaks; Hunspell's built-in default (break at hyphens) isn't applied, so the
//!   languages that check hyphenated words themselves keep doing so.
//!
//! `NEEDAFFIX` stems are words only with an affix; it is read here with the other flags of this
//! family. Searches are bounded: at most [`MAX_PARTS`] parts (`COMPOUNDRULE`: the word's length)
//! and [`PART_BUDGET`] part and `REP` lookups per search (a state that failed once isn't searched
//! again), [`WORD_PART_BUDGET`] for all the searches of one checked word (its case readings and
//! break sides), and [`BREAK_BUDGET`] checks for the breaks of one word, each side checked once.

use super::{AffixDict, Mask, Parse, Rule};

/// Directives read into [`Compounding`].
pub(super) const DIRECTIVES: &[&str] = &[
    "BREAK",
    "CHECKCOMPOUNDCASE",
    "CHECKCOMPOUNDDUP",
    "CHECKCOMPOUNDPATTERN",
    "CHECKCOMPOUNDREP",
    "CHECKCOMPOUNDTRIPLE",
    "COMPOUNDBEGIN",
    "COMPOUNDEND",
    "COMPOUNDFIRST",
    "COMPOUNDFLAG",
    "COMPOUNDFORBIDFLAG",
    "COMPOUNDLAST",
    "COMPOUNDMIDDLE",
    "COMPOUNDMIN",
    "COMPOUNDMORESUFFIXES",
    "COMPOUNDPERMITFLAG",
    "COMPOUNDROOT",
    "COMPOUNDRULE",
    "COMPOUNDSYLLABLE",
    "COMPOUNDWORDMAX",
    "LANG",
    "NEEDAFFIX",
    "ONLYINCOMPOUND",
    "PSEUDOROOT",
    "SYLLABLENUM",
];

/// Directives whose entries follow a count line (`BREAK 3`).
const TABLES: [&str; 3] = ["BREAK", "CHECKCOMPOUNDPATTERN", "COMPOUNDRULE"];

/// Most parts in one compound.
pub(super) const MAX_PARTS: usize = 12;
/// Most part lookups while checking one word for compounds.
pub(super) const PART_BUDGET: u32 = 6000;
/// Most checks spent on the break points of one word.
pub(super) const BREAK_BUDGET: u32 = 64;
/// A word with this many break points or more is wrong, as in Hunspell (`fa-fa-…` with ten
/// hyphens).
const MAX_BREAK_POINTS: usize = 10;

/// Most part lookups for all the compound searches of one checked word (its case readings and
/// break sides together); each search gets at most [`PART_BUDGET`] of them.
pub(super) const WORD_PART_BUDGET: u32 = 16 * PART_BUDGET;

/// The checks of one word: the break budget left, the sides already checked, the part lookups
/// left for the word, and whether a search ran out of them (its "wrong" isn't remembered).
pub(super) struct Breaks {
    budget: u32,
    pub(super) parts: u32,
    pub(super) exhausted: bool,
    pub(super) seen: crate::patterns::FxMap<String, bool>,
}

impl Breaks {
    pub(super) fn new() -> Breaks {
        Breaks { budget: BREAK_BUDGET, parts: WORD_PART_BUDGET, exhausted: false, seen: crate::patterns::FxMap::default() }
    }
}

/// Private-use flags the reader adds to an inflected form listed as a stem: one, two or three
/// syllables more than its lemma (`st:` in its morphology), which the 6–3 rule doesn't count.
const LEMMA_FLAGS: [char; 3] = ['\u{F8F1}', '\u{F8F2}', '\u{F8F3}'];

/// The flag for a stem `extra` syllables longer than its lemma (none for 0).
pub(super) fn lemma_flag(extra: usize) -> Option<char> {
    LEMMA_FLAGS.get(extra.checked_sub(1)?.min(LEMMA_FLAGS.len() - 1)).copied()
}

/// The syllables of `s`: its letters, in lower case, that are among `vowels`.
pub(super) fn count_syllables(vowels: &[char], s: &str) -> usize {
    s.chars().filter(|ch| vowels.contains(&ch.to_lowercase().next().unwrap_or(*ch))).count()
}

/// The vowels of `COMPOUNDSYLLABLE`, when the dictionary counts syllables.
pub(super) fn syllable_vowels(lines: &[String]) -> Option<Vec<char>> {
    lines.iter().find_map(|l| {
        let mut f = l.split(' ');
        (f.next() == Some("COMPOUNDSYLLABLE")).then(|| f.nth(1).unwrap_or("").chars().collect())
    })
}

/// Skips the count line of each table directive: its first line, when that is a number (a
/// trailing comment may follow, as in Hunspell: `BREAK 3 # breaks`).
#[derive(Default)]
pub(super) struct TableHeaders {
    seen: [bool; TABLES.len()],
}

impl TableHeaders {
    /// The directive line to keep, or `None` for a table's count line.
    pub(super) fn line(&mut self, fields: &[&str]) -> Option<String> {
        let dir = fields.first().copied().unwrap_or("");
        if let Some(i) = TABLES.iter().position(|&t| t == dir)
            && let Some(seen) = self.seen.get_mut(i)
            && !*seen
            && fields.get(1).is_some_and(|n| n.parse::<usize>().is_ok())
        {
            *seen = true;
            return None;
        }
        Some(fields.join(" "))
    }
}

/// `lines` with the count line each table needs in front of its entries.
pub(super) fn with_table_headers(lines: &[String]) -> Vec<String> {
    let dir = |l: &String| l.split(' ').next().unwrap_or("").to_string();
    let mut out: Vec<String> = lines.iter().filter(|l| !TABLES.contains(&dir(l).as_str())).cloned().collect();
    for t in TABLES {
        let entries: Vec<&String> = lines.iter().filter(|l| dir(l) == t).collect();
        if !entries.is_empty() {
            out.push(format!("{t} {}", entries.len()));
            out.extend(entries.into_iter().cloned());
        }
    }
    out
}

/// How often a `COMPOUNDRULE` element may repeat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Repeat {
    One,
    Any,
    Optional,
}

/// A `CHECKCOMPOUNDPATTERN`: no part ending in `end` (with `end_flag`, if any) before one
/// starting with `begin` (with `begin_flag`). An `end` of `0` stands for a part that is a stem
/// as listed, without affixes.
#[derive(Clone, Debug)]
struct Pattern {
    end: String,
    end_flag: Mask,
    begin: String,
    begin_flag: Mask,
}

/// The compounding settings of a dictionary (see the module docs).
#[derive(Clone, Debug)]
pub(super) struct Compounding {
    flag: Mask,
    begin: Mask,
    middle: Mask,
    end: Mask,
    pub(super) only_in: Mask,
    permit: Mask,
    forbid: Mask,
    root: Mask,
    pub(super) need_affix: Mask,
    min: usize,
    /// `COMPOUNDMORESUFFIXES`: a part may carry two suffixes in a row (one otherwise).
    more_suffixes: bool,
    word_max: Option<usize>,
    syllable_max: Option<usize>,
    vowels: Box<[char]>,
    syllable_num: Mask,
    /// [`LEMMA_FLAGS`]' bits.
    lemma: [Mask; 3],
    check_dup: bool,
    check_rep: bool,
    check_case: bool,
    check_triple: bool,
    patterns: Vec<Pattern>,
    rules: Vec<Vec<(Mask, Repeat)>>,
    breaks: Vec<String>,
    /// `LANG hu_HU`: the dictionary is Hungarian (see [`AffixDict::part_ok`]).
    hungarian: bool,
}

impl Default for Compounding {
    fn default() -> Self {
        Compounding {
            flag: Mask::EMPTY,
            begin: Mask::EMPTY,
            middle: Mask::EMPTY,
            end: Mask::EMPTY,
            only_in: Mask::EMPTY,
            permit: Mask::EMPTY,
            forbid: Mask::EMPTY,
            root: Mask::EMPTY,
            need_affix: Mask::EMPTY,
            min: 3,
            more_suffixes: false,
            word_max: None,
            syllable_max: None,
            vowels: Box::new([]),
            syllable_num: Mask::EMPTY,
            lemma: [Mask::EMPTY; 3],
            check_dup: false,
            check_rep: false,
            check_case: false,
            check_triple: false,
            patterns: Vec::new(),
            rules: Vec::new(),
            breaks: Vec::new(),
            hungarian: false,
        }
    }
}

/// `text/flag`: the text and the flag's bit (none when there is no `/`).
fn split_flag(field: &str, bit_of: &dyn Fn(char) -> Mask) -> (String, Mask) {
    match field.split_once('/') {
        Some((t, f)) => (t.to_string(), f.chars().fold(Mask::EMPTY, |m, c| m | bit_of(c))),
        None => (field.to_string(), Mask::EMPTY),
    }
}

/// Parse a `COMPOUNDRULE` pattern: flags, each optionally followed by `*` or `?`; a flag may be
/// written in parentheses, `(x)`.
fn parse_rule(text: &str, bit_of: &dyn Fn(char) -> Mask) -> Result<Vec<(Mask, Repeat)>, String> {
    let mut out: Vec<(Mask, Repeat)> = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let flag = match c {
            '*' | '?' => {
                let Some(last) = out.last_mut() else { return Err(format!("COMPOUNDRULE {text}: `{c}` with nothing before it")) };
                last.1 = if c == '*' { Repeat::Any } else { Repeat::Optional };
                continue;
            }
            '(' => {
                let inner: String = chars.by_ref().take_while(|&d| d != ')').collect();
                let mut it = inner.chars();
                match (it.next(), it.next()) {
                    (Some(f), None) => f,
                    _ => return Err(format!("COMPOUNDRULE {text}: only one-character flags are supported")),
                }
            }
            c => c,
        };
        out.push((bit_of(flag), Repeat::One));
    }
    Ok(out)
}

impl Compounding {
    /// Read the directive lines kept by [`TableHeaders`].
    pub(super) fn from_lines(lines: &[String], bit_of: &dyn Fn(char) -> Mask) -> Result<Compounding, String> {
        let mut c = Compounding { lemma: LEMMA_FLAGS.map(bit_of), ..Compounding::default() };
        for line in lines {
            let f: Vec<&str> = line.split(' ').filter(|s| !s.is_empty()).collect();
            let at = |k: usize| f.get(k).copied().unwrap_or("");
            let flags = |s: &str| s.chars().fold(Mask::EMPTY, |m, ch| m | bit_of(ch));
            let number = |s: &str| s.parse::<usize>().map_err(|_| format!("{line}: not a number"));
            match at(0) {
                "COMPOUNDFLAG" => c.flag = flags(at(1)),
                "COMPOUNDBEGIN" | "COMPOUNDFIRST" => c.begin = flags(at(1)),
                "COMPOUNDMIDDLE" => c.middle = flags(at(1)),
                "COMPOUNDEND" | "COMPOUNDLAST" => c.end = flags(at(1)),
                "ONLYINCOMPOUND" => c.only_in = flags(at(1)),
                "COMPOUNDPERMITFLAG" => c.permit = flags(at(1)),
                "COMPOUNDFORBIDFLAG" => c.forbid = flags(at(1)),
                "COMPOUNDROOT" => c.root = flags(at(1)),
                "NEEDAFFIX" | "PSEUDOROOT" => c.need_affix = flags(at(1)),
                "COMPOUNDMIN" => c.min = number(at(1))?.max(1),
                "COMPOUNDMORESUFFIXES" => c.more_suffixes = true,
                "COMPOUNDWORDMAX" => c.word_max = Some(number(at(1))?),
                "COMPOUNDSYLLABLE" => {
                    c.syllable_max = Some(number(at(1))?);
                    c.vowels = at(2).chars().collect();
                }
                "SYLLABLENUM" => c.syllable_num = flags(at(1)),
                "CHECKCOMPOUNDDUP" => c.check_dup = true,
                "CHECKCOMPOUNDREP" => c.check_rep = true,
                "CHECKCOMPOUNDCASE" => c.check_case = true,
                "CHECKCOMPOUNDTRIPLE" => c.check_triple = true,
                // A third field, a simplified spelling the compound may take instead, isn't
                // applied: the spelling the pattern forbids stays wrong, as in Hunspell. Fields
                // after it (a comment) don't count.
                "CHECKCOMPOUNDPATTERN" if f.len() >= 3 => {
                    let (end, end_flag) = split_flag(at(1), bit_of);
                    let (begin, begin_flag) = split_flag(at(2), bit_of);
                    c.patterns.push(Pattern { end, end_flag, begin, begin_flag });
                }
                "COMPOUNDRULE" => c.rules.push(parse_rule(at(1), bit_of)?),
                "BREAK" if !at(1).is_empty() => c.breaks.push(at(1).to_string()),
                "LANG" => c.hungarian = at(1) == "hu" || at(1).starts_with("hu_") || at(1).starts_with("hu-"),
                _ => {}
            }
        }
        Ok(c)
    }

    /// Does the dictionary build compounds at all?
    fn enabled(&self) -> bool {
        !(self.flag | self.begin | self.middle | self.end).is_empty()
    }
}

/// Where a part sits in a compound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Place {
    First,
    Middle,
    Last,
}

/// What the search keeps about an accepted part.
#[derive(Clone, Copy, Debug)]
struct Part {
    /// How many words it counts as (2 for a `COMPOUNDROOT` stem).
    words: usize,
    stem: Mask,
    /// No suffix: the stem ends where the part does (`CHECKCOMPOUNDPATTERN 0`).
    suffixless: bool,
    /// Syllables of its suffixes that don't count toward the 6–3 rule.
    uncounted: usize,
}

/// One compound search: the word, its char boundaries, the parts looked up so far and the
/// states that led nowhere.
struct Search<'w> {
    w: &'w str,
    /// Byte offsets of the char boundaries, `0` and `w.len()` included.
    bounds: Vec<usize>,
    parts: crate::patterns::FxMap<(usize, usize, Place), Option<Part>>,
    /// `(start, start of the previous part, words so far)` of each [`AffixDict::compound_from`]
    /// call that failed: the rest of the word can't be found from there again.
    failed: crate::patterns::FxMap<(usize, usize, usize), ()>,
    budget: u32,
    deny: Mask,
}

impl AffixDict {
    /// Is `w` (normalized, case as written) a compound word?
    pub(super) fn compound_word(&self, w: &str, deny: Mask, budget: &mut u32) -> bool {
        let c = &self.compound;
        if (!c.enabled() && c.rules.is_empty()) || w.len() > super::MAX_WORD_BYTES {
            return false;
        }
        let bounds: Vec<usize> = w.char_indices().map(|(i, _)| i).chain(std::iter::once(w.len())).collect();
        if bounds.len() < 3 {
            return false;
        }
        if *budget == 0 {
            return false;
        }
        let mut s = Search { w, bounds, parts: Default::default(), failed: Default::default(), budget: *budget, deny };
        let found = (c.enabled() && self.compound_from(&mut s, 0, 0, None)) || c.rules.iter().any(|r| self.rule_from(&mut s, r, 0, 0, 0));
        *budget = s.budget;
        found
    }

    /// Can `s.w` from boundary `start` on be the rest of a compound that has `words` words so
    /// far, the last of them `prev` (byte range and part)?
    fn compound_from(&self, s: &mut Search<'_>, start: usize, words: usize, prev: Option<(usize, usize, Part)>) -> bool {
        if words >= MAX_PARTS {
            return false;
        }
        // The previous part ends at `start`, so its start and `words` decide what may follow.
        let state = (start, prev.map_or(usize::MAX, |(a, _, _)| a), words);
        if s.failed.contains_key(&state) {
            return false;
        }
        let found = self.compound_rest(s, start, words, prev);
        if !found {
            s.failed.insert(state, ());
        }
        found
    }

    /// [`AffixDict::compound_from`] without the record of failed states.
    fn compound_rest(&self, s: &mut Search<'_>, start: usize, words: usize, prev: Option<(usize, usize, Part)>) -> bool {
        let c = &self.compound;
        let last_bound = s.bounds.len() - 1;
        for end in (start + c.min)..=last_bound {
            if s.budget == 0 {
                return false;
            }
            let is_last = end == last_bound;
            if start == 0 && is_last {
                continue;
            }
            if !is_last && last_bound - end < c.min {
                continue;
            }
            let (Some(&a), Some(&b)) = (s.bounds.get(start), s.bounds.get(end)) else { continue };
            let place = if start == 0 {
                Place::First
            } else if is_last {
                Place::Last
            } else {
                Place::Middle
            };
            // Every candidate costs one unit, looked up before or not, so the cache can't let a
            // long word try split after split for free.
            s.budget = s.budget.saturating_sub(1);
            let Some(part) = self.part(s, a, b, place) else { continue };
            if let Some((pa, pb, prev_part)) = prev
                && !self.boundary_ok(s.w, (pa, pb, prev_part), (a, b, part), &mut s.budget)
            {
                continue;
            }
            let total = words + part.words;
            if is_last {
                if self.compound_done(s.w, total, part, &mut s.budget) {
                    return true;
                }
            } else if self.compound_from(s, end, total, Some((a, b, part))) {
                return true;
            }
        }
        false
    }

    /// Is `s.w[a..b]` a compound part at `place`? Looked up once per range and place.
    fn part(&self, s: &mut Search<'_>, a: usize, b: usize, place: Place) -> Option<Part> {
        if let Some(p) = s.parts.get(&(a, b, place)) {
            return *p;
        }
        let text = s.w.get(a..b)?;
        let c = &self.compound;
        let mut best: Option<Part> = None;
        let deny = s.deny;
        self.parses(text, &mut |p| {
            if !self.part_ok(p, place, deny) {
                return false;
            }
            // In a Hungarian dictionary a prefix of more than one syllable counts as a word of its
            // own, on any part, as in Hunspell (`kerekasztal` + `elő` + `döntőbíró`).
            let long_prefix = c.hungarian && p.prefix.is_some_and(|r| self.syllables(&r.add) > 1);
            let words = if p.stem.meets(c.root) { 2 } else { 1 } + usize::from(long_prefix);
            let uncounted = if place == Place::Last { self.uncounted_syllables(p) } else { 0 };
            if best.is_none_or(|q| (words, std::cmp::Reverse(uncounted)) < (q.words, std::cmp::Reverse(q.uncounted))) {
                best = Some(Part { words, stem: p.stem, suffixless: p.suffixes[0].is_none(), uncounted });
            }
            // The last part may yet have a reading with more uncounted inflection.
            words == 1 && place != Place::Last
        });
        s.parts.insert((a, b, place), best);
        best
    }

    /// May this parse stand at `place` in a compound?
    fn part_ok(&self, p: &Parse<'_>, place: Place, deny: Mask) -> bool {
        let c = &self.compound;
        if p.stem.meets(self.forbidden | deny) {
            return false;
        }
        if !p.affixed() && p.stem.meets(c.need_affix) || p.outer_cont().meets(c.need_affix) {
            return false;
        }
        // Two suffixes in a row need `COMPOUNDMORESUFFIXES`, except on the last part, which
        // Hunspell checks like a word (`alma` + `váró` + `ban`).
        if place != Place::Last && p.suffixes[1].is_some() && !c.more_suffixes {
            return false;
        }
        let roles = p.stem | p.conts();
        let at_place = match place {
            Place::First => c.begin,
            Place::Middle => c.middle,
            Place::Last => c.end,
        };
        if !roles.meets(c.flag | at_place) {
            return false;
        }
        if place != Place::Last && (p.stem.meets(c.forbid) || p.suffix_rules().any(|r| r.cont.meets(c.forbid))) {
            return false;
        }
        // Inside a compound a suffix needs `COMPOUNDPERMITFLAG`, unless it carries the compound
        // flag itself (`kedvez` + `ő` → `kedvezőpocak`); a prefix on the last part needs the
        // permit flag, as in Hunspell (`anyag` + `ki/Y` `szánt` is wrong, `anyag` + `be/P` `szánt`
        // is right), one on a middle part doesn't (`anyag` + `elszánt` + `ház`). Homonyms are separate parses, so the prefix flag of the verb `szán` doesn't
        // join the compound flag of the noun `szán` (`előanyag` + `elszánt` is wrong).
        let inside = c.permit | c.flag | at_place;
        if place == Place::Last && p.prefix.is_some_and(|r| !r.cont.meets(c.permit)) {
            return false;
        }

        !(place != Place::Last && p.suffix_rules().any(|r: &Rule| !r.cont.meets(inside)))
    }

    /// Syllables of the suffixes on a parse that the 6–3 rule doesn't count: the inflections.
    /// A suffix after which the word may compound again (a compound flag in its continuation
    /// classes: `ápol` + `ás`) forms a new word and counts, as do the classes `SYLLABLENUM`
    /// lists (`-szerű`, `-féle`).
    fn uncounted_syllables(&self, p: &Parse<'_>) -> usize {
        let c = &self.compound;
        let forms_word = c.flag | c.begin | c.middle | c.end | c.syllable_num;
        let lemma = (1..).zip(c.lemma).find(|(_, m)| p.stem.meets(*m)).map_or(0, |(n, _)| n);
        lemma + p.suffix_rules().filter(|r| !r.cont.meets(forms_word) && !r.bit.meets(c.syllable_num)).map(|r| self.syllables(&r.add)).sum::<usize>()
    }

    fn syllables(&self, s: &str) -> usize {
        count_syllables(&self.compound.vowels, s)
    }

    /// May part `(c, d)` follow part `(a, b)`? `CHECKCOMPOUNDREP` spends `budget`.
    fn boundary_ok(&self, w: &str, (a, b, prev): (usize, usize, Part), (c, d, next): (usize, usize, Part), budget: &mut u32) -> bool {
        let cfg = &self.compound;
        let (Some(left), Some(right)) = (w.get(a..b), w.get(c..d)) else { return false };
        if cfg.check_dup && left == right {
            return false;
        }
        // Two parts that together are a listed word are that word (`lég` + `záró` is
        // `légzáró`), which compounds only as its own flags allow.
        let Some(joined) = w.get(a..d) else { return false };
        if self.stem_mask(joined).is_some() {
            return false;
        }
        if cfg.check_rep && self.rep_misspelling(joined, budget) {
            return false;
        }
        let mut l = left.chars().rev();
        let mut r = right.chars();
        let (l1, l2, r1, r2) = (l.next(), l.next(), r.next(), r.next());
        if cfg.check_triple && l1.is_some() && l1 == r1 && (l2 == l1 || r2 == r1) {
            return false;
        }
        if cfg.check_case
            && let (Some(x), Some(y)) = (l1, r1)
            && (x.is_uppercase() || y.is_uppercase())
            && x != '-'
            && y != '-'
        {
            return false;
        }
        !cfg.patterns.iter().any(|p| {
            // `0`: the previous part's stem ends at the boundary, prefixed or not (`el` `kis` +
            // `szék`), as in Hunspell.
            let end = if p.end == "0" {
                prev.suffixless && (p.end_flag.is_empty() || prev.stem.meets(p.end_flag))
            } else {
                left.ends_with(p.end.as_str()) && (p.end_flag.is_empty() || prev.stem.meets(p.end_flag))
            };
            end && right.starts_with(p.begin.as_str()) && (p.begin_flag.is_empty() || next.stem.meets(p.begin_flag))
        })
    }

    /// The checks on a whole compound of `words` words whose last part is `last`.
    fn compound_done(&self, w: &str, words: usize, last: Part, budget: &mut u32) -> bool {
        let c = &self.compound;
        if let Some(max) = c.word_max
            && words > max
        {
            let Some(syllables) = c.syllable_max else { return false };
            if self.syllables(w).saturating_sub(last.uncounted) > syllables {
                return false;
            }
        }
        !(c.check_rep && self.rep_misspelling(w, budget))
    }

    /// Is `w` a `REP` misspelling of a word: does one replacement turn it into a word without
    /// compounding? `^` and `$` tie a pattern to the start or the end. Each word looked up costs
    /// one unit of `budget`; with none left, `w` counts as a misspelling.
    fn rep_misspelling(&self, w: &str, budget: &mut u32) -> bool {
        for (from, to) in &self.rep {
            let (anchored_start, from) = match from.strip_prefix('^') {
                Some(f) => (true, f),
                None => (false, from.as_str()),
            };
            let (anchored_end, from) = match from.strip_suffix('$') {
                Some(f) => (true, f),
                None => (false, from),
            };
            if from.is_empty() {
                continue;
            }
            for (i, _) in w.match_indices(from) {
                if (anchored_start && i != 0) || (anchored_end && i + from.len() != w.len()) {
                    continue;
                }
                let (Some(head), Some(tail)) = (w.get(..i), w.get(i + from.len()..)) else { continue };
                if *budget == 0 {
                    return true;
                }
                *budget -= 1;
                // Any listed stem or affixed reading counts, a forbidden one too, as in
                // Hunspell's `candidate_check`.
                let candidate = format!("{head}{to}{tail}");
                if self.stem_mask(&candidate).is_some() || self.simple_reading(&candidate, Mask::EMPTY).is_some() {
                    return true;
                }
            }
        }
        false
    }

    /// Does `s.w` from boundary `start` on match `rule` from element `at` on, with `parts`
    /// parts so far? Every part is a stem carrying its element's flag; the last may carry
    /// suffixes.
    fn rule_from(&self, s: &mut Search<'_>, rule: &[(Mask, Repeat)], at: usize, start: usize, parts: usize) -> bool {
        let last_bound = s.bounds.len() - 1;
        if start == last_bound {
            return parts >= 2 && rule.get(at..).unwrap_or_default().iter().all(|e| e.1 != Repeat::One);
        }
        let Some(&(flag, repeat)) = rule.get(at) else { return false };
        // Parts are at least `COMPOUNDMIN` letters, as in Hunspell, and as many as the word
        // holds (`1111111111111-es`): each is shorter than the word, so the depth is bounded.
        if s.budget == 0 {
            return false;
        }
        if repeat != Repeat::One && self.rule_from(s, rule, at + 1, start, parts) {
            return true;
        }
        for end in (start + self.compound.min)..=last_bound {
            s.budget = s.budget.saturating_sub(1);
            if s.budget == 0 {
                return false;
            }
            let (Some(&a), Some(&b)) = (s.bounds.get(start), s.bounds.get(end)) else { continue };
            let Some(text) = s.w.get(a..b) else { continue };
            let ok = if end == last_bound {
                self.parses(text, &mut |p| p.prefix.is_none() && p.stem.meets(flag) && !p.stem.meets(self.forbidden | s.deny))
            } else {
                self.stem_mask(text).is_some_and(|m| m.meets(flag) && !m.meets(self.forbidden | s.deny))
            };
            let next = if repeat == Repeat::Any { at } else { at + 1 };
            if ok && self.rule_from(s, rule, next, end, parts + 1) {
                return true;
            }
        }
        false
    }

    /// Is `w` correct when split at a `BREAK` point, both sides checked (each side may break
    /// again)? `^x` and `x$` take `x` off the start or the end.
    pub(super) fn check_breaks(&self, w: &str, deny: Mask, st: &mut Breaks) -> bool {
        let breaks = &self.compound.breaks;
        let anchored = |b: &str| b.starts_with('^') || b.ends_with('$');
        let points: usize = breaks.iter().filter(|b| !b.is_empty() && !anchored(b)).map(|b| w.matches(b.as_str()).count()).sum();
        if points >= MAX_BREAK_POINTS {
            return false;
        }
        for b in breaks {
            if st.budget == 0 {
                return false;
            }
            if let Some(x) = b.strip_prefix('^').filter(|x| !x.is_empty()) {
                if let Some(rest) = w.strip_prefix(x).filter(|r| !r.is_empty()) {
                    st.budget -= 1;
                    if self.side_ok(rest, deny, st) {
                        return true;
                    }
                }
                continue;
            }
            if let Some(x) = b.strip_suffix('$').filter(|x| !x.is_empty()) {
                if let Some(rest) = w.strip_suffix(x).filter(|r| !r.is_empty()) {
                    st.budget -= 1;
                    if self.side_ok(rest, deny, st) {
                        return true;
                    }
                }
                continue;
            }
            for (i, _) in w.match_indices(b.as_str()) {
                let (Some(left), Some(right)) = (w.get(..i), w.get(i + b.len()..)) else { continue };
                if left.is_empty() || right.is_empty() || st.budget == 0 {
                    continue;
                }
                st.budget -= 1;
                // In a Hungarian dictionary the left side of a hyphen may also be a listed word
                // with the hyphen (`vegyiáru-` + `kereskedés`), as in Hunspell.
                let dash = || self.compound.hungarian && b == "-" && left.len() < super::MAX_WORD_BYTES;
                if self.side_ok(right, deny, st) && (self.side_ok(left, deny, st) || (dash() && self.side_ok(&format!("{left}-"), deny, st))) {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::super::AffixDict;

    /// A small dictionary in the shape of the Hungarian one, written for these tests.
    const AFF: &str = "SET UTF-8
FORBIDDENWORD W
COMPOUNDFLAG Y
COMPOUNDBEGIN B
COMPOUNDEND E
COMPOUNDPERMITFLAG P
COMPOUNDFORBIDFLAG F
ONLYINCOMPOUND O
NEEDAFFIX N
COMPOUNDROOT R
COMPOUNDMIN 2
COMPOUNDWORDMAX 2
COMPOUNDSYLLABLE 6 aáeéiíoóöőuúüű
CHECKCOMPOUNDDUP
CHECKCOMPOUNDTRIPLE
CHECKCOMPOUNDCASE
CHECKCOMPOUNDREP
CHECKCOMPOUNDPATTERN 1
CHECKCOMPOUNDPATTERN ssz sz
COMPOUNDRULE 1
COMPOUNDRULE n*m
BREAK 3
BREAK -
BREAK ^-
BREAK -féle$
REP 1
REP jl lj
SFX s Y 2
SFX s 0 ban .
SFX s 0 ok .
SFX d Y 1
SFX d 0 ó/YPs .
SFX i Y 1
SFX i 0 i/F .
";

    const DIC: &str = "40
alma/Ys
fa/Ys
ház/Ysi
tető/Ys
kulcs/Ys
haj/Y
lak/Y
haljak
kis/B
szék/Ys
sakk/Y
kör/Y
háztető/YRs
mikro/OY
vár/Yd
üzelm/Ns
baj/N
baj/Ys
lég/Y
záró/Y
légzáró/s
szezon/Y
rossz/Y
Petőfi/Y
híd/Y
Petőfi-híd/W
almaház/W
SMS
SMS-ben
ipso facto\t1
1/n
9/n
9-es/m
";

    fn dict() -> AffixDict {
        AffixDict::from_hunspell(AFF, DIC, &["test data"]).unwrap()
    }

    fn assert_words(d: &AffixDict, good: &[&str], bad: &[&str]) {
        for w in good {
            assert!(d.check(w), "{w} should be correct");
        }
        for w in bad {
            assert!(!d.check(w), "{w} should be wrong");
        }
    }

    #[test]
    fn compound_flags_and_places() {
        let d = dict();
        assert_words(&d, &["almafa", "almafaban", "házkulcs", "kisház", "almafaház", "Almafa", "ALMAFA"], &["házkis", "almafával", "kulcsx"]);
    }

    #[test]
    fn six_three_rule_counts_syllables_without_inflection() {
        let d = dict();
        // Four words: alma + tető + kulcs + alma has 7 syllables; with the inflection `ban` on
        // the last part, 6 of 7 count.
        assert_words(&d, &["almatetőkulcsház", "almatetőkulcsfaban", "háztetőalma"], &["almatetőkulcsalma"]);
    }

    #[test]
    fn boundaries() {
        let d = dict();
        assert_words(
            &d,
            &["sakkház", "rosszház", "hajház", "lakhaj", "szezonlég", "légzáró"],
            // Duplicate, triple letter, capital at a boundary, `ssz`|`sz`, a REP misspelling of
            // `haljak`, and `lég` + `záró`, which is the listed (non-compounding) `légzáró`.
            &["almaalma", "sakkkör", "almaHáz", "rosszszék", "hajlak", "hajlakház", "szezonlégzáró"],
        );
    }

    #[test]
    fn affixes_inside_compounds() {
        let d = dict();
        // `ó` carries the compound flag and the permit flag; `ban` neither; `i` forbids the
        // first and middle places but may end a compound.
        assert_words(&d, &["váró", "váróház", "házi", "almaházi"], &["házbanalma", "háziház"]);
        // Two suffixes in a row (vár + ó + ban) need COMPOUNDMORESUFFIXES before the last part;
        // the last is checked like a word, as Hunspell does: `almaváróban` is correct.
        assert!(d.check("váróban") && d.check("almaváróban"));
        let more = AffixDict::from_hunspell(&format!("{AFF}COMPOUNDMORESUFFIXES\n"), DIC, &[]).unwrap();
        assert!(more.check("almaváróban"));
    }

    #[test]
    fn only_in_compound_and_need_affix() {
        let d = dict();
        assert_words(&d, &["mikroház", "üzelmok", "baj", "bajok"], &["mikro", "üzelm"]);
    }

    #[test]
    fn first_middle_and_last_places() {
        // `COMPOUNDFIRST` and `COMPOUNDLAST` are the older names of `COMPOUNDBEGIN` and
        // `COMPOUNDEND`; a `COMPOUNDMIDDLE` part sits only between two others.
        let dic = "3\nkis/B\nnagy/M\nház/E\n";
        for (first, last) in [("COMPOUNDBEGIN", "COMPOUNDEND"), ("COMPOUNDFIRST", "COMPOUNDLAST")] {
            let aff = format!("SET UTF-8\n{first} B\nCOMPOUNDMIDDLE M\n{last} E\n");
            let d = AffixDict::from_hunspell(&aff, dic, &[]).unwrap();
            assert_words(&d, &["kisház", "kisnagyház", "kisnagynagyház"], &["kisnagy", "nagyház", "házkis", "kiskis", "nagynagy"]);
        }
    }

    #[test]
    fn pseudoroot_is_needaffix() {
        let aff = AFF.replace("NEEDAFFIX N", "PSEUDOROOT N");
        let d = AffixDict::from_hunspell(&aff, DIC, &[]).unwrap();
        assert_words(&d, &["üzelmok", "üzelmban", "baj", "bajok"], &["üzelm"]);
    }

    #[test]
    fn syllablenum_suffixes_count_toward_the_six_three_rule() {
        // `ban` is inflection and doesn't count, unless `SYLLABLENUM` lists its class.
        let d = dict();
        let counted = AffixDict::from_hunspell(&format!("{AFF}SYLLABLENUM s\n"), DIC, &[]).unwrap();
        assert!(d.check("almatetőkulcsfaban"), "6 of 7 syllables count");
        assert!(!counted.check("almatetőkulcsfaban"), "all 7 count");
        assert!(counted.check("almatetőkulcsfa") && counted.check("almafaban"));
    }

    #[test]
    fn every_compounding_directive_loads() {
        for dir in super::DIRECTIVES {
            let arg = match *dir {
                "COMPOUNDMIN" | "COMPOUNDWORDMAX" => "2",
                "COMPOUNDSYLLABLE" => "6 aeiou",
                "CHECKCOMPOUNDPATTERN" => "ssz sz",
                "COMPOUNDRULE" => "n*m",
                "BREAK" => "-",
                _ => "Y",
            };
            let aff = format!("SET UTF-8\n{dir} {arg}\n");
            assert!(AffixDict::from_hunspell(&aff, "1\nalma/Y\n", &[]).is_ok(), "{dir} should load");
        }
    }

    #[test]
    fn forbidden_words_stay_wrong() {
        let d = dict();
        assert_words(&d, &["Petőfi-ház", "PETŐFI-HÁZ", "házalma"], &["almaház", "Petőfi-híd", "PETŐFI-HÍD"]);
    }

    #[test]
    fn compound_rules_and_breaks() {
        let d = dict();
        assert_words(
            &d,
            // `COMPOUNDMIN 2`: `1999-es` is wrong here, as in Hunspell.
            &["9-es", "alma-ház", "-alma", "alma-féle", "alma-ház-tető", "SMS-ben", "SMS-BEN"],
            &["1999", "1999-es", "19-es", "19x", "alma-xyz", "alma-", "SMS-BAN"],
        );
    }

    #[test]
    fn break_sides_are_checked_like_words() {
        // As Hunspell 1.7 does with the Hungarian dictionary: each side of a break has its own
        // case readings (`alma-Fa`, `Petőfi-HÁZ`), and a word in capitals is split capitalized,
        // so `SÁMBOKI-HAZAY` is `Sámboki-hazay`, which is wrong.
        let d = AffixDict::from_hunspell(AFF, &format!("{DIC}Sámboki/Y\nHazay/Y\n"), &[]).unwrap();
        assert_words(
            &d,
            &["alma-Fa", "Alma-Fa", "ALMA-fa", "alma-FA", "Petőfi-Ház", "Petőfi-HÁZ", "alma-fa-Ház", "Sámboki-Hazay", "SÁMBOKI-Hazay"],
            &["aLMA-fa", "Petőfi-HáZ", "petőfi-ház", "SÁMBOKI-HAZAY", "Sámboki-hazay"],
        );
    }

    #[test]
    fn long_hyphenated_headings_are_checked_within_the_budget() {
        // Every side is checked once per word, so a heading in capitals with many parts is
        // still found correct; ten break points or more make a word wrong, as in Hunspell.
        let d = dict();
        let parts = ["alma", "fa", "ház", "tető", "kulcs", "szék", "alma", "fa", "ház", "tető", "kulcs"];
        assert!(d.check(&parts[..6].join("-").to_uppercase()));
        assert!(d.check(&parts[..10].join("-")) && d.check(&parts[..10].join("-").to_uppercase()), "nine break points");
        assert!(!d.check(&parts.join("-")), "ten break points");
        assert!(!d.check(&format!("{}-xyz", parts[..6].join("-").to_uppercase())));
    }

    #[test]
    fn dictionary_phrases_are_not_words() {
        let d = dict();
        assert!(!d.check("ipso") && !d.check("facto"));
    }

    #[test]
    fn listed_inflections_count_as_their_lemma() {
        // `levelek` is listed with its lemma `levél`: its plural ending is inflection.
        let dic = |morph: &str| format!("4\ntető/Y\nfa/Y\nkulcs/Y\nlevelek/Y{morph}\n");
        let with = AffixDict::from_hunspell(AFF, &dic("\tst:levél ts:PLUR"), &[]).unwrap();
        let without = AffixDict::from_hunspell(AFF, &dic(""), &[]).unwrap();
        let derived = AffixDict::from_hunspell(AFF, &dic("\tst:levél ds:x"), &[]).unwrap();
        assert!(with.check("tetőfakulcslevelek"), "6 of 7 syllables count");
        assert!(!without.check("tetőfakulcslevelek") && !derived.check("tetőfakulcslevelek"));
        let back = AffixDict::from_bytes(&with.to_bytes()).unwrap();
        assert!(back.check("tetőfakulcslevelek"));
    }

    #[test]
    fn compact_format_keeps_compounding() {
        let d = dict();
        let back = AffixDict::from_bytes(&d.to_bytes()).unwrap();
        assert_words(
            &back,
            &["almafa", "kisház", "váróház", "mikroház", "9-es", "alma-ház", "Petőfi-ház", "almatetőkulcsfaban"],
            &["házkis", "sakkkör", "hajlak", "mikro", "üzelm", "Petőfi-híd", "almaház", "almatetőkulcsalma"],
        );
    }

    #[test]
    fn dictionaries_without_compounding_are_unchanged() {
        // No compound flags, no BREAK: neither compounds nor hyphen splits are accepted.
        let d = AffixDict::from_hunspell("SET UTF-8\nSFX a Y 1\nSFX a 0 y .\n", "2\nkot/a\npies/a\n", &[]).unwrap();
        assert!(d.check("koty") && !d.check("kotpies") && !d.check("kot-pies"));
    }

    #[test]
    fn flags_only_an_affix_carries_on_still_count() {
        // `P` (COMPOUNDPERMITFLAG) and `F` (COMPOUNDFORBIDFLAG) appear only after the `/` of a
        // suffix, on no stem and as no rule's own flag; they must still get a bit, or the
        // compound rules that test them would never match.
        let d = AffixDict::from_hunspell(AFF, DIC, &[]).unwrap();
        assert!(d.flag_chars.contains(&'P') && d.flag_chars.contains(&'F'), "{:?}", d.flag_chars);
        let back = AffixDict::from_bytes(&d.to_bytes()).unwrap();
        assert!(back.flag_chars.contains(&'P') && back.flag_chars.contains(&'F'), "the compact format keeps them");
    }

    #[test]
    fn a_word_with_countless_splits_ends_quickly() {
        // Every split of a run of `a`s is a candidate; cached parts must cost budget too, or
        // the search would try split after split for free.
        let d = AffixDict::from_hunspell("SET UTF-8\nCOMPOUNDFLAG Y\nCOMPOUNDMIN 1\n", "1\na/Y\n", &[]).unwrap();
        let w = format!("{}x", "a".repeat(60));
        let t = std::time::Instant::now();
        assert!(!d.check(&w));
        assert!(t.elapsed() < std::time::Duration::from_secs(2), "{:?}", t.elapsed());
    }

    #[test]
    fn failed_states_are_searched_once() {
        // With `COMPOUNDMIN 1` the search meets the same dead ends again and again; once a state
        // has failed it costs nothing more, so the budget lasts until the one valid split, which
        // Hunspell accepts.
        let d = AffixDict::from_hunspell("SET UTF-8\nCOMPOUNDFLAG Y\nCOMPOUNDMIN 1\n", "3\naaa/Y\naaaa/Y\naab/Y\n", &[]).unwrap();
        for n in [40, 42, 44] {
            assert!(d.check(&format!("{}aab", "a".repeat(n))), "{n}");
        }
        assert!(!d.check(&format!("{}aab", "a".repeat(45))), "45 = 3 × 15 needs more than twelve parts");
    }

    #[test]
    fn compound_patterns_as_hunspell_reads_them() {
        // Checked with the Hunspell 1.7 command line: a replacement field and a trailing comment
        // leave the pattern in force, a commented count line is a count, and `0` stands for a
        // part that is a stem as listed.
        let dic = "4\nrossz/Y\nszék/Y\nház/Y\nkis/Y\n";
        let aff =
            |pattern: &str| format!("SET UTF-8\nCOMPOUNDFLAG Y\nCOMPOUNDMIN 2\nCHECKCOMPOUNDPATTERN 1 # count\nCHECKCOMPOUNDPATTERN {pattern}\n");
        for pattern in ["ssz sz", "ssz sz sz", "ssz sz # comment", "ssz sz # comment more"] {
            let d = AffixDict::from_hunspell(&aff(pattern), dic, &[]).unwrap();
            assert_words(&d, &["kisszék", "rosszház", "székrossz"], &["rosszszék"]);
        }
        let d = AffixDict::from_hunspell(&aff("0 sz"), dic, &[]).unwrap();
        assert_words(&d, &["rosszház", "székrossz"], &["rosszszék", "kisszék", "házszék"]);
        let d = AffixDict::from_hunspell(&aff("0/Y sz"), dic, &[]).unwrap();
        assert_words(&d, &["rosszház"], &["kisszék"]);
        let back = AffixDict::from_bytes(&d.to_bytes()).unwrap();
        assert_words(&back, &["rosszház"], &["kisszék"]);
    }

    #[test]
    fn a_commented_count_line_is_no_break() {
        let aff = "SET UTF-8\nBREAK 1 # breaks\nBREAK -\n";
        let d = AffixDict::from_hunspell(aff, "2\nalma\nfa\n", &[]).unwrap();
        assert_words(&d, &["alma-fa"], &["alma1fa", "alma#fa"]);
    }

    #[test]
    fn a_later_prefix_needs_the_permit_flag() {
        // As the Hunspell 1.7 command line reads it, with `LANG hu_HU` or without: on the last
        // part a prefix needs `COMPOUNDPERMITFLAG`, the compound flag on it isn't enough; on a
        // middle part it needs neither (`anyagelszántház`).
        let aff = "SET UTF-8\nCOMPOUNDFLAG Y\nCOMPOUNDPERMITFLAG P\nCOMPOUNDMIN 2\nPFX e Y 1\nPFX e 0 el .\nPFX g Y 1\nPFX g 0 ki/Y .\nPFX h Y 1\nPFX h 0 be/P .\n";
        let dic = "3\nanyag/Yegh\nszánt/Yegh\nház/Y\n";
        for aff in [aff.to_string(), format!("LANG hu_HU\n{aff}")] {
            let d = AffixDict::from_hunspell(&aff, dic, &[]).unwrap();
            assert_words(&d, &["elszánt", "anyagbeszánt", "elanyagszánt", "anyagelszántház"], &["anyagelszánt", "anyagkiszánt"]);
            assert!(!AffixDict::from_bytes(&d.to_bytes()).unwrap().check("anyagkiszánt"));
        }
    }

    #[test]
    fn homonyms_are_parsed_one_by_one() {
        // `szán` the noun compounds, `szán` the verb takes the prefix: `el` + `szán` + `t` is no
        // compound part, as in Hunspell (the Hungarian `előanyagelszánt`).
        let aff = "SET UTF-8\nCOMPOUNDFLAG Y\nCOMPOUNDPERMITFLAG P\nCOMPOUNDMIN 2\nPFX e Y 1\nPFX e 0 el/P .\nSFX t Y 1\nSFX t 0 t .\n";
        let d = AffixDict::from_hunspell(aff, "3\nanyag/Y\nszán/Y\nszán/et\n", &[]).unwrap();
        assert_words(&d, &["szán", "elszán", "elszánt", "anyagszán", "szánt"], &["anyagelszánt", "anyagelszán"]);
        let back = AffixDict::from_bytes(&d.to_bytes()).unwrap();
        assert_words(&back, &["elszánt", "anyagszán"], &["anyagelszánt"]);
    }

    #[test]
    fn hungarian_long_prefixes_and_hyphens() {
        // With `LANG hu_HU`, as in Hunspell: a prefix of two syllables on any part counts as a
        // word for the 6–3 rule, and the left side of a hyphen may be listed with it.
        let aff = "SET UTF-8\nCOMPOUNDFLAG Y\nCOMPOUNDPERMITFLAG P\nCOMPOUNDMIN 2\nCOMPOUNDWORDMAX 2\nCOMPOUNDSYLLABLE 6 aáeéiíoóöőuúüű\nPFX e Y 2\nPFX e 0 elő/P .\nPFX e 0 be/P .\nBREAK 1\nBREAK -\n";
        let dic = "4\nkerekasztal/Y\ndöntőbíró/Ye\nvegyiáru-\nkereskedés\n";
        let plain = AffixDict::from_hunspell(aff, dic, &[]).unwrap();
        assert_words(&plain, &["kerekasztalelődöntőbíró", "kerekasztalbedöntőbíró"], &["vegyiáru-kereskedés", "vegyiáru"]);
        let hu = AffixDict::from_hunspell(&format!("LANG hu_HU\n{aff}"), dic, &[]).unwrap();
        assert_words(&hu, &["kerekasztalbedöntőbíró", "vegyiáru-kereskedés", "VEGYIÁRU-KERESKEDÉS"], &["kerekasztalelődöntőbíró", "vegyiáru"]);
        assert_words(&hu, &["bedöntőbírókerekasztal"], &["elődöntőbírókerekasztal"]);
        assert_words(&plain, &["elődöntőbírókerekasztal"], &[]);
    }

    #[test]
    fn long_hyphenated_compounds_get_a_budget_per_side() {
        // Each break side has its own compound search; a reading that ran out of lookups isn't
        // remembered as wrong. Every side here is a long compound of `COMPOUNDMIN 1` parts.
        let d = AffixDict::from_hunspell("SET UTF-8\nCOMPOUNDFLAG Y\nCOMPOUNDMIN 1\nBREAK 1\nBREAK -\n", "3\naaa/Y\naaaa/Y\naab/Y\n", &[]).unwrap();
        let side = format!("{}aab", "a".repeat(40));
        for k in 2..=4 {
            let w = vec![side.clone(); k].join("-");
            assert!(d.check(&w) && d.check(&w.to_uppercase()) && d.check(&super::super::capitalize(&w)), "{k} sides");
        }
        assert!(!d.check(&format!("{side}-{}x", side)));
    }

    #[test]
    fn forbidden_readings_stop_the_check() {
        // Hunspell: a reading that is a listed FORBIDDENWORD makes the word wrong, before any
        // lower-case reading or break is tried.
        let d = AffixDict::from_hunspell("SET UTF-8\nFORBIDDENWORD W\nBREAK 1\nBREAK -\n", "5\nijs\nIjs/W\nalma\nfa\nalma-fa/W\n", &[]).unwrap();
        assert_words(&d, &["ijs", "alma", "fa"], &["Ijs", "IJS", "IJs", "alma-fa", "Alma-fa", "ALMA-FA"]);
    }

    #[test]
    fn compound_rules_take_any_number_of_parts_of_compoundmin_letters() {
        // Hunspell 1.7: `1` repeated as often as the word holds (60 times here), but no part
        // shorter than `COMPOUNDMIN`: with 2, `1999-es` is wrong.
        let dic = "4\n1/n\n9/n\n0/n\n9-es/m\n";
        let one = AffixDict::from_hunspell("SET UTF-8\nCOMPOUNDMIN 1\nCOMPOUNDRULE 1\nCOMPOUNDRULE n*m\n", dic, &[]).unwrap();
        assert_words(&one, &["1999-es", "19-es", &format!("{}9-es", "1".repeat(60))], &["1999", "1-es"]);
        let two = AffixDict::from_hunspell("SET UTF-8\nCOMPOUNDMIN 2\nCOMPOUNDRULE 1\nCOMPOUNDRULE n*m\n", dic, &[]).unwrap();
        assert_words(&two, &["9-es"], &["1999-es", "19-es"]);
    }

    #[test]
    fn a_zero_pattern_meets_a_prefixed_stem() {
        // `CHECKCOMPOUNDPATTERN 0 sz`: a part whose stem ends at the boundary, prefixed or not,
        // before `sz…` (Hunspell rejects `kisszék` and `elkisszék`, accepts `házosszék`).
        let aff = "SET UTF-8\nCOMPOUNDFLAG Y\nCOMPOUNDPERMITFLAG P\nCOMPOUNDMIN 2\nCHECKCOMPOUNDPATTERN 1\nCHECKCOMPOUNDPATTERN 0 sz\nPFX e Y 1\nPFX e 0 el/P .\nSFX s Y 1\nSFX s 0 os/P .\n";
        let d = AffixDict::from_hunspell(aff, "3\nkis/Ye\nszék/Y\nház/Ys\n", &[]).unwrap();
        assert_words(&d, &["házosszék", "székház"], &["kisszék", "elkisszék", "házszék"]);
    }

    #[test]
    fn compound_rep_checks_spend_the_budget() {
        // Every REP replacement looked up costs budget, so many REP hits at every boundary end
        // quickly.
        let mut aff = String::from("SET UTF-8\nCOMPOUNDFLAG Y\nCOMPOUNDMIN 1\nCHECKCOMPOUNDREP\nREP 200\n");
        for i in 0..200 {
            aff.push_str(&format!("REP a {}\n", "b".repeat(i % 7 + 1)));
        }
        let d = AffixDict::from_hunspell(&aff, "2\na/Y\naa/Y\n", &[]).unwrap();
        let t = std::time::Instant::now();
        let _ = d.check(&format!("{}x", "a".repeat(60)));
        assert!(t.elapsed() < std::time::Duration::from_secs(2), "{:?}", t.elapsed());
    }

    #[test]
    fn forms_leave_out_what_is_no_word() {
        // A bare `NEEDAFFIX` stem and every form of an `ONLYINCOMPOUND` stem aren't words.
        let d = dict();
        assert!(d.forms("mikro").is_empty());
        let forms = d.forms("üzelm");
        assert!(!forms.iter().any(|f| f == "üzelm") && forms.iter().any(|f| f == "üzelmok"), "{forms:?}");
        for f in forms {
            assert!(d.check(&f), "{f}");
        }
    }

    #[test]
    fn forms_leave_out_affixes_that_need_more() {
        // `ó/N`: the form needs another affix; `é/O`: it stands only inside compounds; `ó/Ns`
        // chains to `k`, which is a word again.
        let aff = "SET UTF-8\nNEEDAFFIX N\nONLYINCOMPOUND O\nSFX d Y 1\nSFX d 0 ó/Ns .\nSFX e Y 1\nSFX e 0 é/O .\nSFX s Y 1\nSFX s 0 k .\n";
        let d = AffixDict::from_hunspell(aff, "1\nvár/de\n", &[]).unwrap();
        let forms = d.forms("vár");
        assert!(!forms.iter().any(|f| f == "váró" || f == "váré"), "{forms:?}");
        assert!(forms.iter().any(|f| f == "várók"), "{forms:?}");
        for f in &forms {
            assert!(d.check(f), "{f}");
        }
    }

    #[test]
    fn numbered_morphology_aliases_may_carry_a_comment() {
        // `AM 1 # morphology` is the count line; alias 1 is the first entry after it.
        let aff = format!("{AFF}AM 1 # morphology\nAM st:levél ts:PLUR\n");
        let d = AffixDict::from_hunspell(&aff, "4\ntető/Y\nfa/Y\nkulcs/Y\nlevelek/Y\t1\n", &[]).unwrap();
        assert!(d.check("tetőfakulcslevelek"), "6 of 7 syllables count");
    }

    proptest::proptest! {
        #[test]
        fn compound_lookups_never_panic(s in "[a-zőüöáéí -]{0,60}") {
            let d = dict();
            let _ = d.check(&s);
            let _ = d.check(&s.to_uppercase());
        }
    }
}
