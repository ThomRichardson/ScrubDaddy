//! Embedded name dictionaries and word lists (see tools/build_data.py).

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

static FIRST_TSV: &str = include_str!("../data/first_names.tsv");
static SUR_TSV: &str = include_str!("../data/surnames.tsv");
static COMMON_TXT: &str = include_str!("../data/common_words.txt");

struct Dicts {
    first: HashMap<&'static str, u8>,
    sur: HashMap<&'static str, u8>,
    common: HashMap<&'static str, u32>,
}

fn load_tsv(src: &'static str) -> HashMap<&'static str, u8> {
    let mut m = HashMap::with_capacity(src.len() / 9);
    for line in src.lines() {
        if let Some((n, w)) = line.split_once('\t') {
            m.insert(n, w.trim().parse().unwrap_or(1));
        }
    }
    m
}

fn dicts() -> &'static Dicts {
    static D: OnceLock<Dicts> = OnceLock::new();
    D.get_or_init(|| Dicts {
        first: load_tsv(FIRST_TSV),
        sur: load_tsv(SUR_TSV),
        common: COMMON_TXT
            .lines()
            .enumerate()
            .map(|(i, w)| (w, i as u32 + 1))
            .collect(),
    })
}

/// Warm the dictionaries on a background thread so the first job starts fast.
pub fn preload() {
    std::thread::spawn(|| {
        dicts();
        sets();
    });
}

/// First-name strength 0 (unknown) ..= 3 (very popular).
pub fn first(norm: &str) -> u8 {
    dicts().first.get(norm).copied().unwrap_or(0)
}

/// Surname strength 0 (unknown) ..= 3 (very common surname).
pub fn sur(norm: &str) -> u8 {
    dicts().sur.get(norm).copied().unwrap_or(0)
}

/// Frequency rank among common English words (1 = most common).
pub fn common_rank(norm: &str) -> Option<u32> {
    dicts().common.get(norm).copied()
}

pub struct Sets {
    pub titles: HashSet<&'static str>,
    pub creds: HashSet<&'static str>,
    pub particles: HashSet<&'static str>,
    pub stop: HashSet<&'static str>,
    pub ambiguous: HashSet<&'static str>,
    pub places: HashSet<&'static str>,
    pub org: HashSet<&'static str>,
    pub greet: HashSet<&'static str>,
    pub occupations: HashSet<&'static str>,
}

pub fn sets() -> &'static Sets {
    static S: OnceLock<Sets> = OnceLock::new();
    S.get_or_init(|| Sets {
        titles: TITLES.iter().copied().collect(),
        creds: CREDENTIALS.iter().copied().collect(),
        particles: PARTICLES.iter().copied().collect(),
        stop: STOP.iter().copied().collect(),
        ambiguous: AMBIGUOUS.iter().copied().collect(),
        places: PLACES.iter().copied().collect(),
        org: ORG_WORDS.iter().copied().collect(),
        greet: GREETINGS.iter().copied().collect(),
        occupations: OCCUPATIONS.iter().copied().collect(),
    })
}

const TITLES: &[&str] = &[
    "mr", "mrs", "ms", "miss", "mx", "dr", "doctor", "prof", "professor", "sir", "madam", "dame",
    "rev", "reverend", "fr", "father", "hon", "judge", "justice", "officer", "ofc", "det",
    "detective", "sgt", "sergeant", "lt", "capt", "captain", "col", "cpl", "sr.", "atty", "sra",
    "srta", "señor", "senor", "señora", "senora", "nurse",
];

/// Post-nominal credentials and generational suffixes ("Tim Barker, MS, CDMS").
const CREDENTIALS: &[&str] = &[
    "jr", "sr", "ii", "iii", "iv", "md", "do", "phd", "psyd", "dc", "pt", "dpt", "ot", "otr",
    "otr/l", "rn", "lpn", "np", "arnp", "aprn", "pa", "pa-c", "ms", "ma", "ba", "bs", "bsn",
    "msn", "msw", "lcsw", "lmhc", "lpc", "lmft", "med", "edd", "esq", "jd", "cpa", "cdms", "crc",
    "vrc", "vrci", "ccm", "cpe", "cie", "mba", "mph", "mpa", "faan", "facp", "faps", "abpn",
    "dabpn", "atc", "cscs", "cwce", "cfce", "cee", "cvrc", "lmp", "lmt", "dds", "dmd", "od",
    "pharmd", "rph", "facs", "faaos", "faapmr", "mmsc", "abvf", "abve", "dabfe", "lics",
];

const PARTICLES: &[&str] = &[
    "van", "von", "der", "den", "de", "del", "della", "di", "da", "dos", "das", "du", "la", "le",
    "st", "ibn", "bin", "al", "el", "ter", "ten", "y", "mac",
];

const GREETINGS: &[&str] = &["dear", "hi", "hello", "hey", "attn", "attention", "thanks"];

/// Words that are never part of a person's name in running text.
const STOP: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "for", "to", "in", "on", "at", "by", "with", "from",
    "as", "is", "are", "was", "were", "be", "been", "this", "that", "these", "those", "his",
    "her", "he", "she", "they", "their", "them", "it", "its", "we", "our", "you", "your", "my",
    "me", "i", "not", "no", "yes", "all", "any", "re", "cc", "bcc", "dear", "sincerely",
    "regards", "respectfully", "thank", "thanks", "please", "page", "date", "report", "claim",
    "claims", "case", "number", "worker", "workers", "name", "client", "patient", "employer",
    "employee", "provider", "physician", "clinic", "hospital", "center", "medical", "health",
    "department", "dept", "office", "division", "county", "state", "city", "street", "avenue",
    "ave", "road", "rd", "drive", "suite", "box", "po", "inc", "llc", "ltd", "corp", "company",
    "group", "services", "university", "college", "school", "insurance", "labor", "industries",
    "vocational", "rehabilitation", "counselor", "assessment", "evaluation", "plan", "job",
    "analysis", "signature", "phone", "fax", "email", "address", "dob", "doi", "ssn", "january",
    "february", "march", "april", "may", "june", "july", "august", "september", "october",
    "november", "december", "jan", "feb", "mar", "apr", "jun", "jul", "aug", "sep", "sept",
    "oct", "nov", "dec", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday",
    "sunday", "united", "states", "america", "american", "north", "south", "east", "west",
    "northern", "southern", "eastern", "western", "right", "left", "upper", "lower", "total",
    "subject", "summary", "history", "diagnosis", "impression", "assessment", "recommendations",
    "recommendation", "conclusion", "conclusions", "exam", "examination", "independent",
    "functional", "capacity", "attending", "treating", "chief", "complaint", "section", "part",
    "table", "figure", "appendix", "exhibit", "attachment", "enclosure", "enclosures", "note",
    "notes", "per", "via", "about", "after", "before", "during", "under", "over", "between",
    "into", "through", "will", "shall", "should", "would", "could", "can", "may", "might",
    "must", "if", "then", "than", "but", "so", "because", "while", "when", "where", "who",
    "what", "which", "how", "why", "also", "only", "just", "very", "more", "most", "less",
    "other", "same", "such", "each", "every", "both", "either", "neither", "none", "some",
    "unknown", "none", "n/a", "na", "tbd", "pending", "approved", "denied", "accepted",
    "examinee", "claimant", "applicant", "injured", "doctor", "therapist", "evaluator",
    "examiner", "adjuster", "attorney", "nurse", "program", "formcheckbox", "formtext",
    "formdropdown", "mergefield", "hyperlink", "fillin", "numpages", "pageref", "macrobutton",
    "seldom", "occasional", "occasionally", "frequent", "frequently", "constant", "constantly",
    "never", "lifting", "carrying", "pushing", "pulling", "resting", "pulse", "pain", "acronym",
    "key", "word", "outlook", "excel", "windows", "microsoft", "google", "indeed", "goodwill",
    "good", "deal", "post", "pre", "injury", "injuries",
    // Medical / ICD condition vocabulary and its abbreviations.
    "sprain", "strain", "strn", "unspecified", "unsp", "uns", "specified", "init", "enc",
    "initial", "encounter", "subsequent", "sequela", "rt", "lt", "bilat", "bilateral", "arm",
    "arms", "shldr", "shoulder", "shoulders", "levl", "level", "wrist", "wrists", "hand",
    "hands", "elbow", "knee", "knees", "ankle", "foot", "feet", "hip", "hips", "neck", "back",
    "spine", "lumbar", "thoracic", "cervical", "musc", "muscle", "fasc", "fascia", "tend",
    "tendon", "forearm", "finger", "thumb", "ring", "index", "little", "trigger", "carpal",
    "tunnel", "syndrome", "fracture", "fx", "dspl", "clos", "tear", "rotator", "cuff",
    "disorder", "derangement", "derangements", "joint", "joints", "nec", "nos", "contusion",
    "laceration", "dislocation", "radiculopathy", "stenosis", "herniation", "disc", "displaced",
    "nondisplaced", "styloid", "process", "ulna", "radius", "upper", "limb", "limbs", "region",
    "therapy", "physical", "occupational", "chiropractic", "chiropractor", "surgery",
    "surgeon", "orthopedic", "orthopedics", "neurology", "psychiatry", "psychology",
    "psychologist", "psychiatrist", "radiology", "mri", "x-ray", "ct", "emg", "ncs",
    // Physical demands in job analyses and FCEs.
    "climb", "climbing", "stairs", "stair", "ladder", "ladders", "stoop", "stooping",
    "kneel", "kneeling", "crawl", "crawling", "crouch", "crouching", "squat", "squatting",
    "reach", "reaching", "balance", "balancing", "twist", "twisting", "bend", "bending",
    "walk", "walking", "stand", "standing", "sit", "sitting", "carry", "push", "pull",
    "handle", "handling", "grasp", "grasping", "fingering", "feeling", "overhead", "waist",
    "floor", "keyboarding", "vibration", "vibrations", "conveyor", "forklift", "pallet",
];

/// Job titles that are also surnames; common in vocational and medical reports.
const OCCUPATIONS: &[&str] = &[
    "butcher", "cutter", "barber", "carpenter", "framer", "painter", "driver", "laborer",
    "plumber", "roofer", "welder", "cashier", "clerk", "server", "farmer", "fisher", "hunter",
    "baker", "cook", "mason", "miller", "porter", "taylor", "tailor", "packer", "sorter",
    "loader", "picker", "stocker", "janitor", "custodian", "cleaner", "installer", "mechanic",
    "machinist", "operator", "assembler", "inspector", "supervisor", "manager", "foreman",
    "guard", "teller", "nurse", "aide", "assistant", "associate", "helper", "trimmer",
    "logger", "faller", "rigger", "glazier", "drywaller", "electrician", "fitter", "grinder",
    "sander", "polisher", "shipper", "receiver", "dispatcher", "waiter", "bartender",
    "dishwasher", "housekeeper", "caregiver", "landscaper", "gardener", "groundskeeper",
];

/// Popular names that are also everyday words; they need context to count as names.
const AMBIGUOUS: &[&str] = &[
    "will", "may", "mark", "grant", "bill", "rose", "hope", "june", "april", "august", "white",
    "brown", "green", "black", "young", "long", "hall", "king", "rich", "frank", "art", "guy",
    "joy", "faith", "grace", "page", "chase", "case", "park", "field", "wood", "woods", "hill",
    "stone", "lane", "banks", "wells", "rice", "cross", "west", "north", "south", "ward", "best",
    "little", "short", "major", "miles", "sharp", "strong", "hunt", "fox", "gray", "grey",
    "bishop", "cook", "baker", "early", "english", "lord", "story", "house", "street", "mill",
    "rivers", "price", "bell", "love", "cash", "sky", "summer", "dawn", "eve", "iris", "lily",
    "ivy", "amber", "crystal", "ruby", "pearl", "jade", "robin", "rocky", "river", "storm",
    "sunny", "dean", "jack", "ray", "don", "pat", "sue", "gene", "earl", "duke", "prince",
    "penny", "sandy", "misty", "candy", "carol", "chip", "chuck", "cliff", "glen", "dale",
    "drew", "lance", "buck", "hunter", "mason", "taylor", "cooper", "carter", "parker", "turner",
    "walker", "miller", "fisher", "smith", "porter", "foster", "golden", "silver", "gold",
    "summers", "winter", "spring", "christian", "noble", "royal", "max", "sterling", "justice",
    "harmony", "destiny", "trinity", "precious", "unique", "genesis", "heaven", "journey",
    "melody", "serenity", "charity", "honor", "liberty", "patience", "mercy", "brook", "brooks",
    "forest", "ocean", "rain", "star", "angel", "baby", "lucky", "major", "deputy", "sergeant",
    "colonel", "general", "booker", "worker", "reed", "reid", "head", "hand", "foot", "arm",
    "back", "neck", "knee", "hip", "work", "job", "home", "office", "center", "court", "judge",
];

/// Places that commonly show up in these documents and also look like names.
const PLACES: &[&str] = &[
    "washington", "olympia", "seattle", "tacoma", "everett", "spokane", "kelso", "vancouver",
    "bellevue", "kent", "renton", "yakima", "lacey", "tumwater", "bellingham", "kennewick",
    "pasco", "richland", "wenatchee", "aberdeen", "longview", "bremerton", "lynnwood",
    "redmond", "kirkland", "auburn", "federal", "puyallup", "marysville", "shoreline",
    "edmonds", "burien", "tukwila", "centralia", "chehalis", "shelton", "sequim", "port",
    "angeles", "oregon", "portland", "salem", "eugene", "idaho", "boise", "california",
    "georgia", "virginia", "carolina", "florida", "texas", "arizona", "nevada", "montana",
    "alaska", "hawaii", "dakota", "indiana", "louisiana", "maryland", "jersey", "york",
    "mexico", "canada", "jordan", "paris", "london", "austin", "dallas", "houston", "denver",
    "phoenix", "orlando", "aurora", "charlotte", "madison", "lincoln", "jackson", "chelsea",
    "victoria", "florence", "sydney", "regina", "columbia", "pacific", "atlantic", "pierce",
    "king", "snohomish", "thurston", "clark", "cowlitz", "lewis", "mason", "grays", "harbor",
    "lewis", "skagit", "whatcom", "benton", "franklin", "grant", "chelan", "kitsap", "clallam",
];

/// Words that follow a capitalized run when it is an organization or address, not a person.
const ORG_WORDS: &[&str] = &[
    "inc", "llc", "ltd", "llp", "pllc", "pc", "corp", "corporation", "co", "company",
    "construction", "clinic", "clinics", "hospital", "center", "centre", "university",
    "college", "school", "foundation", "group", "associates", "services", "law", "firm",
    "bank", "county", "street", "st", "ave", "avenue", "road", "rd", "blvd", "boulevard", "way",
    "drive", "dr", "lane", "ln", "court", "ct", "place", "pl", "highway", "hwy", "building",
    "institute", "department", "church", "district", "medical", "health", "healthcare",
    "physical", "therapy", "rehab", "rehabilitation", "orthopedics", "orthopaedics",
    "chiropractic", "imaging", "radiology", "pharmacy", "industries", "enterprises",
    "partners", "agency", "consulting", "solutions", "systems", "international", "trucking",
    "logistics", "farms", "meats", "restaurant", "market", "store", "stores", "motors",
];
