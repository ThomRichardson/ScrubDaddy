//! Person-name and case-number detection.
//!
//! Works on plain lines of text (from a PDF text layer, OCR, or DOCX paragraphs):
//! 1. Tokenize, keeping punctuation flags (comma/colon/period) and capitalization shape.
//! 2. Find capitalized runs and score them as names using first-name/surname dictionaries,
//!    titles (Mr., Dr.), credentials (", MS, CDMS"), field labels ("Worker Name:") and
//!    "Last, First" ordering; assign each token a role (first/middle/last).
//! 3. Group names into people, then propagate: every later occurrence of any known name part
//!    (any case, possessive, hyphenated, or one OCR typo away) is redacted too.
//! 4. Case/claim numbers: two letters + 5-8 digits (BD78735), or anything after a claim label.

use crate::dict::{self, sets};
use std::collections::{HashMap, HashSet};

pub struct Line {
    pub text: Vec<char>,
    /// Starts a new paragraph/cell; wrapped PDF lines are not hard boundaries.
    pub para_start: bool,
    /// Came from OCR, so tolerate recognition errors.
    pub ocr: bool,
}

impl Line {
    pub fn new(text: &str, para_start: bool, ocr: bool) -> Self {
        Line { text: text.chars().collect(), para_start, ocr }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Role {
    First,
    Middle,
    Last,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Name(Role, Option<u32>),
    CaseNumber,
    Email,
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
}

impl Hit {
    pub fn label(&self) -> String {
        match self.kind {
            Kind::CaseNumber => "[CASE #]".into(),
            Kind::Email => "[EMAIL]".into(),
            Kind::Name(role, person) => {
                let r = match role {
                    Role::First => "FIRST",
                    Role::Middle => "MIDDLE",
                    Role::Last => "LAST",
                };
                match person {
                    Some(id) => format!("[P{id} {r}]"),
                    None => format!("[{r} NAME]"),
                }
            }
        }
    }
}

pub struct Detection {
    pub hits: Vec<Hit>,
    pub people: usize,
    pub names: usize,
    pub case_numbers: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    Title,
    Upper,
    Initial,
    Lower,
    Other,
}

#[derive(Debug)]
struct Tok {
    line: usize,
    start: usize,
    end: usize,
    text: String,
    norm: String,
    shape: Shape,
    comma: bool,
    colon: bool,
    period: bool,
    paren: bool,
    email: bool,
    line_first: bool,
    sent_start: bool,
}

const SEP: &[char] = &[
    ':', ';', '/', '\\', '(', ')', '[', ']', '{', '}', '"', '|', '<', '>', ',', '“', '”', '•',
    '·', '*', '=', '+', '#', '–', '—', '_', '~',
];

fn is_ws(c: char) -> bool {
    c.is_whitespace() || c == '\u{a0}' || c == '\u{200b}'
}

fn normalize(s: &str) -> String {
    s.chars()
        .map(|c| if c == '’' || c == '`' { '\'' } else { c })
        .flat_map(char::to_lowercase)
        .collect()
}

fn shape_of(s: &str) -> Shape {
    let (mut letters, mut upper, mut lower) = (0, 0, 0);
    for c in s.chars() {
        if c.is_alphabetic() {
            letters += 1;
            if c.is_uppercase() {
                upper += 1;
            } else if c.is_lowercase() {
                lower += 1;
            }
        } else if !matches!(c, '-' | '\'' | '’' | '.') {
            return Shape::Other;
        }
    }
    if letters == 0 {
        return Shape::Other;
    }
    let first_up = s.chars().next().is_some_and(char::is_uppercase);
    if letters == 1 {
        return if first_up { Shape::Initial } else { Shape::Lower };
    }
    if lower == 0 {
        Shape::Upper
    } else if first_up {
        Shape::Title
    } else if upper == 0 {
        Shape::Lower
    } else {
        Shape::Other
    }
}

fn make_tok(line: usize, t: &[char], a: usize, b: usize, email: bool) -> Tok {
    let text: String = t[a..b].iter().collect();
    Tok {
        line,
        start: a,
        end: b,
        norm: normalize(&text),
        shape: if email { Shape::Other } else { shape_of(&text) },
        text,
        comma: false,
        colon: false,
        period: false,
        paren: false,
        email,
        line_first: false,
        sent_start: false,
    }
}

fn tokenize(lines: &[Line]) -> Vec<Tok> {
    let mut out = Vec::new();
    for (li, line) in lines.iter().enumerate() {
        let t = &line.text;
        let first = out.len();
        let mut i = 0;
        while i < t.len() {
            while i < t.len() && is_ws(t[i]) {
                i += 1;
            }
            let s = i;
            while i < t.len() && !is_ws(t[i]) {
                i += 1;
            }
            if s < i {
                chunk(li, t, s, i, &mut out);
            }
        }
        if out.len() > first {
            out[first].line_first = true;
        }
    }
    let s = sets();
    for k in 0..out.len() {
        out[k].sent_start = if k == 0 {
            true
        } else {
            let p = &out[k - 1];
            let cur = &out[k];
            if p.line != cur.line {
                lines[cur.line].para_start || p.period || p.colon
            } else {
                (p.period && p.shape != Shape::Initial && !s.titles.contains(p.norm.as_str()))
                    || p.colon
            }
        };
    }
    out
}

fn chunk(li: usize, t: &[char], s: usize, e: usize, out: &mut Vec<Tok>) {
    if t[s..e].contains(&'@') {
        let (mut a, mut b) = (s, e);
        while a < b && !t[a].is_alphanumeric() {
            a += 1;
        }
        while b > a && !t[b - 1].is_alphanumeric() {
            b -= 1;
        }
        if a < b {
            out.push(make_tok(li, t, a, b, true));
        }
        return;
    }
    let mut ps = s;
    let mut prev: Option<char> = None;
    for k in s..=e {
        let sep = if k == e { None } else { Some(t[k]).filter(|c| SEP.contains(c)) };
        if k == e || sep.is_some() {
            piece(li, t, ps, k, prev, sep, out);
            prev = sep;
            ps = k + 1;
        }
    }
}

fn piece(li: usize, t: &[char], mut a: usize, mut b: usize, prev: Option<char>, next: Option<char>, out: &mut Vec<Tok>) {
    while a < b && !t[a].is_alphanumeric() {
        a += 1;
    }
    let (mut period, mut comma, mut colon) = (false, false, false);
    while b > a && !t[b - 1].is_alphanumeric() {
        match t[b - 1] {
            '.' | '!' | '?' | ';' => period = true,
            ',' => comma = true,
            ':' => colon = true,
            _ => {}
        }
        b -= 1;
    }
    if a >= b {
        return;
    }
    let mut core_end = b;
    if b - a > 3 && matches!(t[b - 1], 's' | 'S') && matches!(t[b - 2], '\'' | '’') {
        core_end = b - 2;
    }
    match next {
        Some(',') => comma = true,
        Some(':') => colon = true,
        Some(';') => period = true,
        _ => {}
    }
    let mut tok = make_tok(li, t, a, core_end, false);
    tok.period = period;
    tok.comma = comma;
    tok.colon = colon;
    tok.paren = prev == Some('(') || next == Some(')');
    out.push(tok);
}

/// Split hyphenated names ("Gutierrez-Lopez") into parts for dictionary lookups.
fn parts(norm: &str) -> impl Iterator<Item = &str> {
    norm.split(['-', '.']).filter(|p| p.len() >= 2)
}

#[derive(Clone, Copy, PartialEq)]
enum Pos {
    First,
    Middle,
    Last,
    Single,
}

#[derive(Clone, Copy, Default)]
struct LabelInfo {
    name: bool,
    case: bool,
    role: Option<Role>,
}

struct Span {
    toks: Vec<usize>,
    nick: Vec<usize>,
    roles: Vec<Role>,
}

struct Ctx<'a> {
    lines: &'a [Line],
    toks: &'a [Tok],
    lower: HashMap<&'a str, u32>,
    upper_line: Vec<bool>,
}

impl<'a> Ctx<'a> {
    fn new(lines: &'a [Line], toks: &'a [Tok]) -> Self {
        let mut lower: HashMap<&str, u32> = HashMap::new();
        let mut counts = vec![(0u32, 0u32); lines.len()];
        for t in toks {
            if t.shape == Shape::Lower && !t.email {
                *lower.entry(t.norm.as_str()).or_default() += 1;
            }
            if matches!(t.shape, Shape::Title | Shape::Upper | Shape::Lower) {
                counts[t.line].1 += 1;
                if t.shape == Shape::Upper {
                    counts[t.line].0 += 1;
                }
            }
        }
        let upper_line = counts.iter().map(|&(u, n)| n > 0 && u * 10 >= n * 7).collect();
        Ctx { lines, toks, lower, upper_line }
    }

    /// How much a token reads as an ordinary word in this document (0 = not at all .. 3).
    fn wordness(&self, norm: &str) -> f32 {
        if norm.contains('-') {
            return norm.split('-').filter(|p| !p.is_empty()).map(|p| self.wordness(p)).fold(0.0, f32::max);
        }
        if sets().occupations.contains(norm) {
            return 2.0;
        }
        let base = match dict::common_rank(norm) {
            Some(r) if r <= 1000 => 3.0,
            Some(r) if r <= 3000 => 2.0,
            Some(_) => 1.0,
            None => 0.0,
        };
        if self.lower.get(norm).copied().unwrap_or(0) > 0 {
            return f32::max(base, 2.0);
        }
        if base > 0.0
            && !sets().ambiguous.contains(norm)
            && (dict::first(norm) >= 2 || dict::sur(norm) >= 3 && base <= 1.0)
        {
            return 0.0;
        }
        base
    }

    fn fs(&self, t: &Tok) -> f32 {
        let whole = dict::first(&t.norm);
        parts(&t.norm).map(dict::first).max().unwrap_or(0).max(whole) as f32
    }

    fn ss(&self, t: &Tok) -> f32 {
        let whole = dict::sur(&t.norm);
        parts(&t.norm).map(dict::sur).max().unwrap_or(0).max(whole) as f32
    }

    /// Cannot be any part of a name.
    fn bad(&self, i: usize) -> bool {
        let t = &self.toks[i];
        let s = sets();
        if t.email || !matches!(t.shape, Shape::Title | Shape::Upper | Shape::Initial) {
            return true;
        }
        let n = t.norm.trim_end_matches('.');
        if s.stop.contains(n) || s.titles.contains(n) {
            return true;
        }
        if t.shape == Shape::Upper && s.creds.contains(n) {
            return true;
        }
        // Short all-caps tokens are acronyms (VRC, ATW, FCE) unless they are common names.
        if t.shape == Shape::Upper && t.text.chars().count() <= 4 && dict::first(n) < 2 && dict::sur(n) < 2 {
            return true;
        }
        if t.shape == Shape::Upper && t.text.chars().count() <= 3 && !self.upper_line[t.line] {
            return true;
        }
        // Pluralized acronyms ("IMEs") and similar mixed-case codes.
        let mut cs = t.text.chars();
        if let (Some(a), Some(b)) = (cs.next(), cs.next()) {
            if a.is_uppercase() && b.is_uppercase() && t.text.chars().any(char::is_lowercase) {
                return true;
            }
        }
        // Hyphenated phrases ("Non-Eligible", "Pre-Test") where a piece is an ordinary word.
        if t.norm.contains('-')
            && t.norm.split('-').any(|p| p.len() < 2 || self.wordness(p) > 0.0 || s.stop.contains(p))
        {
            return true;
        }
        false
    }

    fn val(&self, i: usize, pos: Pos) -> f32 {
        let t = &self.toks[i];
        if t.shape == Shape::Initial {
            return match pos {
                Pos::Last | Pos::Single => -5.0,
                Pos::First => 0.4,
                Pos::Middle => 1.0,
            };
        }
        let s = sets();
        if t.shape == Shape::Lower && s.particles.contains(t.norm.as_str()) {
            return 0.3;
        }
        let (fs, ss) = (self.fs(t), self.ss(t));
        let w = self.wordness(&t.norm);
        let place = if s.places.contains(t.norm.as_str()) { 2.0 } else { 0.0 };
        let has_vowel = t.norm.chars().any(|c| "aeiouy".contains(c));
        let unknown = if fs == 0.0 && ss == 0.0 && w == 0.0 && t.text.chars().count() >= 4 && has_vowel {
            0.8
        } else {
            0.0
        };
        let v = match pos {
            Pos::First => fs.max(ss * 0.6),
            Pos::Last => ss.max(fs * 0.6),
            Pos::Middle | Pos::Single => fs.max(ss),
        };
        v.max(unknown) - w * if pos == Pos::Last { 0.7 } else { 1.0 } - place
    }

    fn next_in_line(&self, i: usize) -> Option<&Tok> {
        self.toks.get(i + 1).filter(|n| n.line == self.toks[i].line)
    }

    fn org_after(&self, last: usize) -> bool {
        self.next_in_line(last)
            .is_some_and(|n| sets().org.contains(n.norm.trim_end_matches('.')) && n.shape != Shape::Lower)
    }

    /// The run is followed by ", MS, CDMS" or ", MD" style credentials.
    fn creds_after(&self, last: usize) -> bool {
        if !self.toks[last].comma {
            return false;
        }
        self.next_in_line(last).is_some_and(|n| {
            let c = n.norm.trim_end_matches('.').replace('.', "");
            sets().creds.contains(c.as_str()) && n.shape != Shape::Lower
        })
    }

    /// Field label in front of token `i`: "Worker Name: ___", "Claim #: ___", or a label on the
    /// previous line/table cell.
    fn label_before(&self, i: usize) -> LabelInfo {
        let t = &self.toks[i];
        if i > 0 {
            let p = &self.toks[i - 1];
            if p.line == t.line && sets().greet.contains(p.norm.as_str()) {
                return LabelInfo { name: true, ..Default::default() };
            }
        }
        let line = &self.lines[t.line].text;
        let before: String = line[..t.start].iter().collect();
        let trimmed = before.trim_end();
        let seg = if trimmed.ends_with([':', '#']) {
            let body = trimmed.trim_end_matches([':', '#', ' ', '.']);
            body.rsplit([':', ';', '\t']).next().unwrap_or("").to_string()
        } else if trimmed.chars().all(|c| !c.is_alphanumeric()) && t.line > 0 {
            // A label in the previous table cell/line only applies to a short value line.
            if self.lines[t.line].text.len() > 48 {
                return LabelInfo::default();
            }
            let prev: String = self.lines[t.line - 1].text.iter().collect();
            let prev = prev.trim();
            if prev.chars().count() > 60 {
                return LabelInfo::default();
            }
            let low = prev.to_lowercase();
            let low = low.trim_end_matches([':', '#', ' ', ',', '.']);
            const ENDINGS: &[&str] = &[
                "name", "signature", "sincerely", "respectfully", "regards", "claimant", "client",
                "worker", "patient", "examinee", "claim number", "claim", "case number", "case",
            ];
            if !(prev.ends_with(':') || ENDINGS.iter().any(|w| low.ends_with(w))) {
                return LabelInfo::default();
            }
            low.to_string()
        } else {
            return LabelInfo::default();
        };
        classify_label(&seg.to_lowercase())
    }

    fn line_mostly_upper(&self, i: usize) -> bool {
        self.upper_line[self.toks[i].line]
    }
}

fn has_word(seg: &str, w: &str) -> bool {
    seg.split(|c: char| !c.is_alphanumeric() && c != '&').any(|x| x == w)
}

fn classify_label(seg: &str) -> LabelInfo {
    const NAME: &[&str] = &[
        "name", "client", "worker", "patient", "claimant", "employee", "applicant", "insured",
        "re", "attn", "attention", "to", "from", "cc", "author", "writer", "counselor", "vrc",
        "physician", "doctor", "provider", "therapist", "examiner", "evaluator", "signed",
        "prepared", "reviewed", "contact", "adjuster", "nurse", "attorney", "ap", "dr",
        "clinician", "member", "sincerely", "regards", "respectfully", "by", "manager",
        "supervisor", "spouse", "parent", "guardian", "witness", "plaintiff", "defendant",
        "petitioner", "respondent", "student", "tenant", "recipient", "sender", "customer",
        "representative", "interpreter", "translator", "examinee", "injured", "ime", "md",
    ];
    const NOT_NAME: &[&str] = &[
        "employer", "company", "firm", "facility", "clinic", "address", "phone", "date",
        "title", "job", "program", "school", "tool", "tools", "code", "occupation", "diagnosis",
        "business", "insurance", "street", "city", "unit", "age", "wages", "hours", "type",
        "status", "outcome", "narrative", "fax", "email", "e-mail", "branch", "location",
    ];
    let mut info = LabelInfo::default();
    if seg.contains("claim") || seg.contains("case") || has_word(seg, "file") {
        if !seg.contains("unit") && !seg.contains("manager") {
            info.case = true;
        }
    }
    if NAME.iter().any(|w| has_word(seg, w)) && !NOT_NAME.iter().any(|w| has_word(seg, w)) {
        info.name = true;
        if seg.contains("first name") || seg.contains("given name") {
            info.role = Some(Role::First);
        } else if seg.contains("last name") || seg.contains("surname") || seg.contains("family name") {
            info.role = Some(Role::Last);
        } else if seg.contains("middle") {
            info.role = Some(Role::Middle);
        }
    }
    info
}

/// Two letters + 5..8 digits ("BD78735", "BL-77802"), tolerating OCR letter/digit swaps.
fn is_case_number(s: &str, ocr: bool) -> bool {
    let c: Vec<char> = s.chars().filter(|&c| c != '-').collect();
    if c.len() < 7 || c.len() > 10 {
        return false;
    }
    let letter = |x: char| x.is_ascii_uppercase();
    if !(letter(c[0]) && letter(c[1])) {
        return false;
    }
    let mut digits = 0;
    let mut fixes = 0;
    for &x in &c[2..] {
        if x.is_ascii_digit() {
            digits += 1;
        } else if ocr && matches!(x, 'O' | 'o' | 'I' | 'l' | 'S' | 'B' | 'Z') {
            fixes += 1;
        } else {
            return false;
        }
    }
    digits >= 5 && fixes <= 1 && digits + fixes <= 8
}

fn levenshtein_le(a: &str, b: &str, max: usize) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return false;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        let mut row_min = cur[0];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            row_min = row_min.min(cur[j]);
        }
        if row_min > max {
            return false;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()] <= max
}

#[derive(Default)]
struct Person {
    first: HashSet<String>,
    last: HashSet<String>,
    order: usize,
}

pub fn detect(lines: &[Line]) -> Detection {
    let toks = tokenize(lines);
    let ctx = Ctx::new(lines, &toks);
    let s = sets();

    // ---------- 1. capitalized runs -> scored name spans ----------
    let mut spans: Vec<(Span, bool /*title*/)> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let line = toks[i].line;
        let mut title = false;
        let mut title_idx = None;
        // Titles ("Dr.", "Mr") start a run.
        if s.titles.contains(toks[i].norm.trim_end_matches('.'))
            && (toks[i].shape == Shape::Title || toks[i].shape == Shape::Upper && ctx.upper_line[line])
            && toks.get(i + 1).is_some_and(|n| n.line == line && !ctx.bad(i + 1))
        {
            title = true;
            title_idx = Some(i);
            i += 1;
        }
        if ctx.bad(i) {
            i += 1;
            continue;
        }
        // Collect the run.
        let mut run = Vec::new();
        let mut nick = Vec::new();
        let mut j = i;
        while j < toks.len() && toks[j].line == line && run.len() + nick.len() < 6 {
            let t = &toks[j];
            let particle = t.shape == Shape::Lower
                && s.particles.contains(t.norm.as_str())
                && !run.is_empty()
                && toks.get(j + 1).is_some_and(|n| n.line == line && !ctx.bad(j + 1));
            if !particle && ctx.bad(j) {
                break;
            }
            if t.paren && !run.is_empty() {
                nick.push(j);
            } else {
                run.push(j);
            }
            j += 1;
            let ends = t.comma || t.colon || (t.period && t.shape != Shape::Initial);
            if ends {
                break;
            }
        }
        let start_for_label = title_idx.unwrap_or(i);
        let label = ctx.label_before(start_for_label);
        if run.is_empty() {
            i = j.max(i + 1);
            continue;
        }

        // "Last, First [Middle]" ordering.
        let last_tok = *run.last().unwrap();
        if !title && run.len() <= 2 && toks[last_tok].comma && !ctx.creds_after(last_tok) {
            let mut b = Vec::new();
            let mut k = last_tok + 1;
            while k < toks.len() && toks[k].line == line && b.len() < 2 && !ctx.bad(k) {
                b.push(k);
                if toks[k].comma || toks[k].period && toks[k].shape != Shape::Initial {
                    break;
                }
                k += 1;
            }
            if !b.is_empty() && run.len() == 1 {
                let a = run[0];
                let surname = ctx.val(a, Pos::Last);
                let given = ctx.val(b[0], Pos::First);
                let mid = if b.len() > 1 { ctx.val(b[1], Pos::Middle) } else { 0.0 };
                let mut sc = surname + given + mid.min(1.0);
                if label.name {
                    sc += 3.0;
                }
                let caps = toks[a].shape == Shape::Upper && toks[b[0]].shape == Shape::Upper;
                let given_ok = ctx.fs(&toks[b[0]]) >= if caps { 1.0 } else { 2.0 };
                if surname >= 1.0 && given >= 1.0 && given_ok && mid >= -0.5 && sc >= 4.0 {
                    let mut toks_v = vec![a];
                    toks_v.extend(&b);
                    let mut roles = vec![Role::Last, Role::First];
                    if b.len() > 1 {
                        roles.push(Role::Middle);
                    }
                    spans.push((Span { toks: toks_v, nick: vec![], roles }, false));
                    i = b.last().unwrap() + 1;
                    continue;
                }
            }
        }

        if let Some(span) = best_spans(&ctx, &run, nick, title, label) {
            for sp in span {
                spans.push((sp, title));
            }
        }
        i = j.max(i + 1);
    }

    // ---------- 2. people ----------
    let mut people: Vec<Person> = Vec::new();
    let mut span_person: Vec<Option<usize>> = Vec::new();
    // Full names first so titles/single names can attach to them.
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|&k| {
        let sp = &spans[k].0;
        let full = sp.roles.contains(&Role::First) && sp.roles.contains(&Role::Last);
        (!full, sp.toks[0])
    });
    span_person.resize(spans.len(), None);
    for &k in &order {
        let sp = &spans[k].0;
        let firsts: Vec<String> = role_norms(&toks, sp, Role::First);
        let lasts: Vec<String> = role_norms(&toks, sp, Role::Last);
        let found = people.iter().position(|p| {
            let last_ok = lasts.iter().any(|l| p.last.contains(l));
            let first_ok = firsts.iter().any(|f| p.first.contains(f));
            if !lasts.is_empty() && !firsts.is_empty() {
                last_ok && (first_ok || p.first.is_empty())
            } else if !lasts.is_empty() {
                last_ok
            } else {
                first_ok
            }
        });
        let pi = match found {
            Some(pi) => pi,
            None => {
                if firsts.is_empty() && lasts.is_empty() {
                    continue;
                }
                people.push(Person { order: sp.toks[0], ..Default::default() });
                people.len() - 1
            }
        };
        people[pi].first.extend(firsts);
        people[pi].last.extend(lasts);
        people[pi].order = people[pi].order.min(sp.toks[0]);
        span_person[k] = Some(pi);
    }
    // Number people by first appearance.
    let mut by_order: Vec<usize> = (0..people.len()).collect();
    by_order.sort_by_key(|&p| people[p].order);
    let mut pid = vec![0u32; people.len()];
    for (n, &p) in by_order.iter().enumerate() {
        pid[p] = n as u32 + 1;
    }

    // ---------- 3. hits from spans, then propagation ----------
    let mut hit_tok: HashMap<usize, Kind> = HashMap::new();
    // part -> (person, role) votes
    let mut part_map: HashMap<String, Vec<(Option<usize>, Role)>> = HashMap::new();
    for (k, (sp, _)) in spans.iter().enumerate() {
        let person = span_person[k];
        for (n, &ti) in sp.toks.iter().enumerate() {
            let role = sp.roles[n];
            hit_tok.insert(ti, Kind::Name(role, person.map(|p| pid[p])));
            if toks[ti].shape != Shape::Initial && toks[ti].norm.chars().count() >= 2 {
                part_map.entry(toks[ti].norm.clone()).or_default().push((person, role));
            }
        }
        for &ti in &sp.nick {
            hit_tok.insert(ti, Kind::Name(Role::First, person.map(|p| pid[p])));
            part_map.entry(toks[ti].norm.clone()).or_default().push((person, Role::First));
        }
    }
    // Resolve each part to a single role/person.
    let resolved: HashMap<String, Kind> = part_map
        .iter()
        .map(|(part, votes)| {
            let mut role_count: HashMap<Role, usize> = HashMap::new();
            let mut persons: HashSet<usize> = HashSet::new();
            for (p, r) in votes {
                *role_count.entry(*r).or_default() += 1;
                if let Some(p) = p {
                    persons.insert(*p);
                }
            }
            // Fall back to whoever owns this part in the people table.
            if persons.is_empty() {
                for (pi, p) in people.iter().enumerate() {
                    if p.first.contains(part) || p.last.contains(part) {
                        persons.insert(pi);
                    }
                }
            }
            let role = role_count.into_iter().max_by_key(|&(_, c)| c).map(|(r, _)| r).unwrap();
            let person = if persons.len() == 1 { persons.into_iter().next().map(|p| pid[p]) } else { None };
            (part.clone(), Kind::Name(role, person))
        })
        .filter(|(p, _)| !s.stop.contains(p.as_str()) && !s.creds.contains(p.as_str()))
        .collect();
    let fuzzy_parts: Vec<&String> = resolved.keys().filter(|p| p.chars().count() >= 5).collect();

    let mut case_numbers = 0usize;
    for (ti, t) in toks.iter().enumerate() {
        if hit_tok.contains_key(&ti) {
            continue;
        }
        let ocr = lines[t.line].ocr;
        // Case / claim numbers.
        if is_case_number(&t.text, ocr) {
            hit_tok.insert(ti, Kind::CaseNumber);
            continue;
        }
        let digits = t.text.chars().filter(char::is_ascii_digit).count();
        if digits >= 4 && t.text.chars().count() >= 5 && t.text.chars().count() <= 16 && !t.text.contains('/') {
            let lab = ctx.label_before(ti);
            if lab.case && !lab.name {
                hit_tok.insert(ti, Kind::CaseNumber);
                continue;
            }
        }
        if t.email {
            let local = t.norm.split('@').next().unwrap_or("");
            let named = local
                .split(|c: char| !c.is_alphabetic())
                .any(|p| p.len() >= 3 && (resolved.contains_key(p) || resolved.keys().any(|k| k.len() >= 4 && p.contains(k.as_str()))));
            if named {
                hit_tok.insert(ti, Kind::Email);
            }
            continue;
        }
        if matches!(t.shape, Shape::Other | Shape::Initial) {
            continue;
        }
        let mut cands: Vec<&str> = vec![t.norm.as_str()];
        cands.extend(t.norm.split('-').filter(|p| p.len() >= 2));
        let plural = t.norm.strip_suffix('s').filter(|p| p.len() >= 4);
        cands.extend(plural);
        let mut found = None;
        for c in &cands {
            if let Some(k) = resolved.get(*c) {
                if t.shape == Shape::Lower && (ctx.wordness(c) > 0.0 || s.stop.contains(*c)) {
                    continue;
                }
                found = Some(*k);
                break;
            }
        }
        if found.is_none()
            && matches!(t.shape, Shape::Title | Shape::Upper)
            && t.norm.chars().count() >= 5
            && ctx.wordness(&t.norm) == 0.0
            && !s.stop.contains(t.norm.as_str())
        {
            let max = if ocr && t.norm.chars().count() >= 8 { 2 } else { 1 };
            for p in &fuzzy_parts {
                if levenshtein_le(&t.norm, p, max) {
                    found = resolved.get(*p).copied();
                    break;
                }
            }
        }
        if let Some(k) = found {
            hit_tok.insert(ti, k);
        }
    }
    // Grow names into name-like neighbours ("Corina [Nistor]" -> both), then propagate the
    // newly learned parts across the document.
    let mut new_parts: HashMap<String, Kind> = HashMap::new();
    loop {
        let mut added = false;
        for ti in 0..toks.len() {
            let Some(Kind::Name(role, person)) = hit_tok.get(&ti).copied() else { continue };
            let mut cands = Vec::new();
            if ti > 0 && role != Role::First {
                cands.push((ti - 1, Role::First));
            }
            if role != Role::Last {
                cands.push((ti + 1, Role::Last));
            }
            for (nb, nrole) in cands {
                if nb >= toks.len() || hit_tok.contains_key(&nb) || toks[nb].line != toks[ti].line {
                    continue;
                }
                let left = &toks[nb.min(ti)];
                if left.comma || left.colon || (left.period && left.shape != Shape::Initial) {
                    continue;
                }
                if ctx.bad(nb) || toks[nb].shape == Shape::Initial || toks[nb].paren {
                    continue;
                }
                let t = &toks[nb];
                let dict_ok = if nrole == Role::First { ctx.fs(t) >= 1.0 } else { ctx.ss(t) >= 1.0 };
                if ctx.wordness(&t.norm) == 0.0
                    && !s.places.contains(t.norm.as_str())
                    && (dict_ok || ctx.val(nb, Pos::Middle) >= 0.8)
                {
                    let k = Kind::Name(nrole, person);
                    hit_tok.insert(nb, k);
                    new_parts.insert(t.norm.clone(), k);
                    added = true;
                }
            }
        }
        if !added {
            break;
        }
    }
    for (ti, t) in toks.iter().enumerate() {
        if !hit_tok.contains_key(&ti) && t.shape != Shape::Lower {
            if let Some(k) = new_parts.get(&t.norm) {
                hit_tok.insert(ti, *k);
            }
        }
    }
    // Middle initials sandwiched between redacted name parts ("Nazario P Infante").
    for ti in 1..toks.len().saturating_sub(1) {
        if toks[ti].shape == Shape::Initial
            && !hit_tok.contains_key(&ti)
            && toks[ti - 1].line == toks[ti].line
            && toks[ti + 1].line == toks[ti].line
            && matches!(hit_tok.get(&(ti - 1)), Some(Kind::Name(..)))
            && matches!(hit_tok.get(&(ti + 1)), Some(Kind::Name(..)))
        {
            let person = match hit_tok[&(ti - 1)] {
                Kind::Name(_, p) => p,
                _ => None,
            };
            hit_tok.insert(ti, Kind::Name(Role::Middle, person));
        }
    }

    let mut hits: Vec<Hit> = hit_tok
        .into_iter()
        .map(|(ti, kind)| {
            if kind == Kind::CaseNumber {
                case_numbers += 1;
            }
            Hit { line: toks[ti].line, start: toks[ti].start, end: toks[ti].end, kind }
        })
        .collect();
    hits.sort_by_key(|h| (h.line, h.start));
    let names = hits.iter().filter(|h| matches!(h.kind, Kind::Name(..))).count();
    Detection { hits, people: people.len(), names, case_numbers }
}

fn role_norms(toks: &[Tok], sp: &Span, role: Role) -> Vec<String> {
    sp.toks
        .iter()
        .zip(&sp.roles)
        .filter(|(&t, &r)| r == role && toks[t].shape != Shape::Initial)
        .map(|(&t, _)| toks[t].norm.clone())
        .collect()
}

/// Score a capitalized run; if the whole run is not a name, look for the best name-shaped
/// sub-spans inside it ("Terry Martin Construction" -> "Terry Martin").
fn best_spans(ctx: &Ctx, run: &[usize], nick: Vec<usize>, title: bool, label: LabelInfo) -> Option<Vec<Span>> {
    let whole = score(ctx, run, title, label);
    if whole.is_some() {
        return Some(vec![make_span(ctx, run, nick, title, label)]);
    }
    if run.len() < 2 {
        return None;
    }
    let mut out = Vec::new();
    let mut a = 0;
    while a < run.len() {
        let mut best: Option<(usize, f32)> = None;
        for len in (1..=run.len().min(4)).rev() {
            if a + len > run.len() || len == run.len() {
                continue;
            }
            let sub = &run[a..a + len];
            // Titles and labels only bind to the start of the run.
            let (t, l) = if a == 0 { (title, label) } else { (false, LabelInfo::default()) };
            if let Some(sc) = score(ctx, sub, t, l) {
                if best.is_none_or(|(_, b)| sc > b) {
                    best = Some((len, sc));
                }
            }
        }
        match best {
            Some((len, _)) => {
                let sub = &run[a..a + len];
                let (t, l) = if a == 0 { (title, label) } else { (false, LabelInfo::default()) };
                out.push(make_span(ctx, sub, vec![], t, l));
                a += len;
            }
            None => a += 1,
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn score(ctx: &Ctx, run: &[usize], title: bool, label: LabelInfo) -> Option<f32> {
    let n = run.len();
    let toks = ctx.toks;
    let last = *run.last().unwrap();
    let creds = ctx.creds_after(last);
    let org = ctx.org_after(last);
    if n == 1 {
        let i = run[0];
        let t = &toks[i];
        if t.shape == Shape::Initial {
            return None;
        }
        let v = ctx.val(i, Pos::Single);
        let w = ctx.wordness(&t.norm);
        if title {
            return (w < 3.0 || ctx.fs(t) + ctx.ss(t) > 0.0).then_some(3.0 + v);
        }
        if (label.name || creds) && v >= 0.3 && (ctx.fs(t) >= 1.0 || ctx.ss(t) >= 1.0) {
            return Some(3.0 + v);
        }
        if org || sets().places.contains(t.norm.as_str()) || w > 0.0 {
            return None;
        }
        let (fs, ss) = (ctx.fs(t), ctx.ss(t));
        if fs >= 3.0 || (ss >= 2.0 && fs == 0.0 && t.norm.chars().count() >= 4) {
            return Some(v);
        }
        return None;
    }
    let mut total = 0.0;
    let mut minv = f32::MAX;
    for (k, &i) in run.iter().enumerate() {
        let pos = if k == 0 { Pos::First } else if k == n - 1 { Pos::Last } else { Pos::Middle };
        let v = ctx.val(i, pos);
        total += v;
        minv = minv.min(v);
    }
    if title {
        total += 3.0;
    }
    if label.name {
        total += 3.0;
    }
    if creds {
        total += 3.0;
    }
    if org {
        total -= 1.5;
    }
    let first = &toks[run[0]];
    if first.sent_start && ctx.wordness(&first.norm) > 0.0 {
        total -= 1.0;
    }
    if ctx.line_mostly_upper(run[0]) && !(title || label.name || creds) {
        // Headings in all caps need strong dictionary support on every token.
        if run.iter().any(|&i| ctx.fs(&toks[i]).max(ctx.ss(&toks[i])) < 1.0 && toks[i].shape != Shape::Initial) {
            return None;
        }
    }
    let need = 3.5 + (n as f32 - 2.0);
    let floor = if title || label.name || creds { -1.5 } else { -0.5 };
    (total >= need && minv >= floor).then_some(total)
}

fn make_span(ctx: &Ctx, run: &[usize], nick: Vec<usize>, title: bool, label: LabelInfo) -> Span {
    let toks = ctx.toks;
    let n = run.len();
    let mut roles = Vec::with_capacity(n);
    if n == 1 {
        let t = &toks[run[0]];
        let (fs, ss) = (ctx.fs(t), ctx.ss(t));
        let r = if let Some(r) = label.role {
            r
        } else if title {
            if fs >= 2.0 && ss <= 1.0 { Role::First } else { Role::Last }
        } else if fs >= ss {
            Role::First
        } else {
            Role::Last
        };
        roles.push(r);
    } else if title && n == 2 && ctx.fs(&toks[run[0]]) < 2.0 && toks[run[0]].shape != Shape::Initial {
        // "Mr. Mota Soriano": compound surname.
        roles.extend([Role::Last, Role::Last]);
    } else {
        let mut in_last = false;
        for (k, &i) in run.iter().enumerate() {
            let t = &toks[i];
            let r = if k == 0 {
                Role::First
            } else if k == n - 1 || in_last {
                Role::Last
            } else if t.shape == Shape::Lower {
                in_last = true;
                Role::Last
            } else if t.shape == Shape::Initial {
                Role::Middle
            } else if ctx.ss(t) > ctx.fs(t) && ctx.fs(t) < 2.0 {
                // Second surname ("Rigoberto Trujillo Ramirez").
                in_last = true;
                Role::Last
            } else {
                Role::Middle
            };
            roles.push(r);
        }
    }
    Span { toks: run.to_vec(), nick, roles }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &[&str]) -> Vec<(String, String)> {
        let lines: Vec<Line> = text.iter().map(|t| Line::new(t, true, false)).collect();
        let d = detect(&lines);
        d.hits
            .iter()
            .map(|h| (lines[h.line].text[h.start..h.end].iter().collect(), h.label()))
            .collect()
    }

    fn redacted(text: &[&str]) -> Vec<String> {
        run(text).into_iter().map(|(t, _)| t).collect()
    }

    #[test]
    fn full_names_and_roles() {
        let r = run(&["Worker Name: Rebeca Gonzalez", "Ms. Gonzalez injured her foot."]);
        assert_eq!(r[0], ("Rebeca".into(), "[P1 FIRST]".into()));
        assert_eq!(r[1], ("Gonzalez".into(), "[P1 LAST]".into()));
        assert_eq!(r[2], ("Gonzalez".into(), "[P1 LAST]".into()));
    }

    #[test]
    fn credentials_nicknames_and_compound_surnames() {
        let r = redacted(&[
            "Assigned VRC Name: Ashraf (Tray) Sweis, CDMS, VRC",
            "Tim Barker, MS, CDMS",
            "Client: Rigoberto Trujillo Ramirez",
            "Mr. Trijillo Ramirez was on light duty.",
        ]);
        for w in ["Ashraf", "Tray", "Sweis", "Tim", "Barker", "Rigoberto", "Trujillo", "Ramirez", "Trijillo"] {
            assert!(r.contains(&w.to_string()), "missing {w}: {r:?}");
        }
        assert!(!r.iter().any(|w| w == "CDMS" || w == "VRC" || w == "MS"));
    }

    #[test]
    fn case_numbers() {
        let r = run(&["Claim #: BL77802", "Olympia WA 98504-4291", "former claim, BF40685."]);
        let cases: Vec<_> = r.iter().filter(|(_, l)| l == "[CASE #]").map(|(t, _)| t.clone()).collect();
        assert_eq!(cases, vec!["BL77802", "BF40685"]);
    }

    #[test]
    fn not_names() {
        let r = redacted(&[
            "Department of Labor and Industries",
            "The worker is able to work as: Customer Service Representative",
            "R Carpal Tunnel Syndrome Right Upper Limb",
            "May 15, 2026",
            "Job Search Tools Used: Indeed",
        ]);
        assert!(r.is_empty(), "{r:?}");
    }

    #[test]
    fn last_first_and_letters() {
        let r = redacted(&["GONZALEZ, REBECA", "Dear Dr. Nistor,", "Sincerely,", "Mary Richardson, CDMS, VRC"]);
        for w in ["GONZALEZ", "REBECA", "Nistor", "Mary", "Richardson"] {
            assert!(r.contains(&w.to_string()), "missing {w}: {r:?}");
        }
    }

    #[test]
    fn grows_into_neighbours() {
        let r = redacted(&["Corina Nistor", "The Everett Clinic", "Dear Dr. Nistor,"]);
        assert!(r.contains(&"Corina".to_string()), "{r:?}");
        assert!(!r.contains(&"Everett".to_string()), "{r:?}");
    }

    #[test]
    fn emails_with_names() {
        let r = run(&["My name is Mary Richardson.", "Contact me at mrichardson@penvoc.com today."]);
        assert!(r.iter().any(|(t, l)| t == "mrichardson@penvoc.com" && l == "[EMAIL]"), "{r:?}");
    }
}

/// Developer aid: when SCRUBDADDY_DEBUG names a file, append every line with redactions marked.
pub fn debug_dump(lines: &[Line], det: &Detection) {
    let Some(path) = std::env::var_os("SCRUBDADDY_DEBUG") else { return };
    let mut out = String::new();
    let mut hi = det.hits.iter().peekable();
    for (li, l) in lines.iter().enumerate() {
        let mut pos = 0;
        while let Some(h) = hi.peek().filter(|h| h.line == li) {
            out.extend(&l.text[pos..h.start]);
            out.push_str(&format!("⟦{}|{}⟧", l.text[h.start..h.end].iter().collect::<String>(), h.label()));
            pos = h.end;
            hi.next();
        }
        out.extend(&l.text[pos..]);
        out.push('\n');
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(out.as_bytes());
    }
}
