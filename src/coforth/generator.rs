/// Co-Forth English library generator.
///
/// Drives the AI to produce a validated Forth snippet for every English word,
/// saving results to `src/coforth/english_library.toml` (baked in at compile
/// time) or `~/.finch/library.toml` (user extension, loaded at runtime).

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};

// ── Word categories — the English language ────────────────────────────────────

pub const CATEGORIES: &[(&str, &[&str])] = &[
    ("numbers", &[
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
        "ten", "eleven", "twelve", "thirteen", "fourteen", "fifteen", "sixteen",
        "seventeen", "eighteen", "nineteen", "twenty", "thirty", "forty", "fifty",
        "sixty", "seventy", "eighty", "ninety", "hundred", "thousand", "million",
        "billion", "trillion", "infinity", "zero", "unit", "dozen", "pair", "half",
        "quarter", "third", "tenth", "percent",
    ]),
    ("arithmetic", &[
        "add", "subtract", "multiply", "divide", "remainder", "power", "root",
        "logarithm", "factorial", "fibonacci", "sum", "product", "difference",
        "quotient", "average", "mean", "median", "mode", "range", "variance",
        "deviation", "absolute", "modulo", "ceiling", "floor", "round", "truncate",
        "increment", "decrement", "double", "halve", "negate", "reciprocal",
        "square", "cube", "triangle-number", "tetrahedral", "prime", "composite",
        "even", "odd", "positive", "negative", "integer", "fraction", "ratio",
        "decimal", "binary", "hexadecimal", "octal",
    ]),
    ("logic", &[
        "true", "false", "negate", "conjunction", "disjunction", "implication",
        "equivalence", "contradiction", "tautology", "valid", "consistent",
        "satisfiable", "provable", "axiom", "theorem", "lemma", "corollary",
        "premise", "conclusion", "hypothesis", "assertion", "negation",
        "converse", "contrapositive", "biconditional", "exclusive",
    ]),
    ("comparison", &[
        "equal", "unequal", "greater", "lesser", "maximum", "minimum",
        "compare", "rank", "threshold", "boundary", "limit",
        "above", "below", "within", "outside", "clamp", "between",
        "larger", "smaller", "higher", "lower", "closest", "farthest",
        "ascending", "descending", "sorted", "reversed",
    ]),
    ("sequences", &[
        "sequence", "series", "list", "array", "first", "last", "next", "previous",
        "position", "index", "length", "empty", "iterate", "accumulate",
        "monotone", "periodic", "bounded", "prefix", "suffix", "subsequence",
        "tally", "enumerate", "zip", "interleave", "chunk", "window",
        "head", "tail", "rest", "cons", "append", "prepend",
    ]),
    ("sets", &[
        "set", "element", "member", "subset", "superset", "union", "intersection",
        "difference", "complement", "partition", "disjoint", "cardinality",
        "empty-set", "singleton", "universe", "cover", "contains", "belongs",
    ]),
    ("functions", &[
        "function", "domain", "codomain", "range", "compose", "identity", "inverse",
        "apply", "inject", "surject", "biject", "fixed-point",
        "image", "preimage", "monotone", "constant", "linear", "affine",
        "partial", "total", "recursive", "iterative",
    ]),
    ("geometry", &[
        "point", "line", "plane", "circle", "triangle", "rectangle", "polygon",
        "area", "perimeter", "radius", "diameter", "angle", "distance",
        "parallel", "perpendicular", "intersect", "rotate", "scale",
        "vector", "magnitude", "direction", "dimension", "volume", "surface",
        "convex", "concave", "symmetric", "origin", "slope", "gradient",
        "tangent", "normal", "arc", "chord", "sector", "segment",
        "hypotenuse", "sine", "cosine", "adjacent", "opposite",
    ]),
    ("topology", &[
        "open", "closed", "connected", "compact", "continuous",
        "boundary", "interior", "closure", "dense", "neighborhood",
        "converge", "diverge", "limit", "metric", "homeomorphic",
        "contractible", "homotopy", "manifold", "surface", "embedding",
    ]),
    ("algebra", &[
        "group", "ring", "field", "module", "vector-space", "algebra",
        "homomorphism", "isomorphism", "automorphism", "kernel", "image",
        "quotient", "ideal", "subgroup", "coset", "orbit",
        "generator", "relation", "presentation", "free", "abelian",
    ]),
    ("category-theory", &[
        "object", "morphism", "functor", "natural-transformation",
        "adjunction", "monad", "comonad", "endofunctor",
        "product", "coproduct", "equalizer", "pullback", "pushout",
        "terminal", "initial", "exponential", "topos",
    ]),
    ("time", &[
        "now", "past", "future", "before", "after", "during", "moment",
        "duration", "interval", "cycle", "period", "frequency",
        "early", "late", "simultaneous", "sequential", "concurrent",
        "instant", "epoch", "elapsed", "deadline", "schedule",
        "second", "minute", "hour", "day", "week", "month", "year",
    ]),
    ("space", &[
        "here", "there", "near", "far", "inside", "outside", "above", "below",
        "left", "right", "forward", "backward", "center", "edge",
        "position", "location", "coordinate", "origin", "direction",
        "horizontal", "vertical", "diagonal", "depth", "width", "height",
    ]),
    ("language", &[
        "symbol", "letter", "digit", "word", "token", "parse", "encode", "decode",
        "translate", "interpret", "compile", "evaluate", "quote",
        "literal", "identifier", "keyword", "grammar", "syntax",
        "semantics", "pragma", "comment", "string", "character",
        "alphabet", "vocabulary", "dictionary", "index", "lookup",
    ]),
    ("mind", &[
        "remember", "forget", "imagine", "believe", "doubt",
        "choose", "focus", "attention", "intention", "perception",
        "intuition", "inference", "deduction", "induction", "analogy",
        "recognize", "classify", "abstract", "generalize", "specialize",
        "learn", "unlearn", "teach", "model", "predict", "expect",
    ]),
    ("action", &[
        "create", "destroy", "start", "stop", "continue", "pause",
        "repeat", "move", "transform", "combine", "split", "merge",
        "connect", "disconnect", "send", "receive", "store", "retrieve",
        "process", "return", "apply", "reduce", "expand", "contract",
        "push", "pull", "lift", "drop", "rotate", "flip", "copy",
        "cut", "paste", "insert", "delete", "update", "replace",
        "open", "close", "lock", "unlock", "enable", "disable",
    ]),
    ("properties", &[
        "size", "weight", "speed", "strength", "fast", "slow",
        "big", "small", "high", "low", "deep", "shallow",
        "full", "dense", "sparse", "uniform", "random",
        "stable", "volatile", "mutable", "immutable", "pure", "impure",
        "finite", "infinite", "countable", "discrete", "continuous",
        "linear", "nonlinear", "smooth", "rough", "flat", "curved",
        "regular", "irregular", "periodic", "aperiodic",
    ]),
    ("relationships", &[
        "depend", "independent", "similar", "different", "opposite",
        "contain", "overlap", "associate", "commute",
        "distribute", "factor", "compose", "decompose",
        "embed", "project", "lift", "lower", "refine", "coarsen",
        "before", "after", "causes", "follows", "implies", "requires",
    ]),
    ("computing", &[
        "bit", "byte", "register", "memory", "address", "pointer",
        "stack", "queue", "heap", "tree", "graph", "node", "edge",
        "path", "cycle", "hash", "cache", "buffer", "stream",
        "thread", "process", "signal", "interrupt", "exception",
        "branch", "loop", "condition", "variable", "constant",
        "type", "value", "reference", "mutable", "immutable",
        "serialize", "deserialize", "compress", "encrypt",
    ]),
    ("physics", &[
        "mass", "force", "energy", "momentum", "velocity", "acceleration",
        "gravity", "friction", "pressure", "temperature", "entropy",
        "wave", "frequency", "amplitude", "resonance", "decay",
        "charge", "field", "particle", "photon", "electron",
        "potential", "kinetic", "thermal", "electric", "magnetic",
        "quantum", "spin", "orbital", "nucleus", "atom", "molecule",
    ]),
    ("biology", &[
        "cell", "gene", "protein", "organism", "species", "population",
        "evolve", "adapt", "reproduce", "grow", "differentiate",
        "receptor", "pathway", "network", "feedback", "regulation",
        "metabolism", "energy", "membrane", "nucleus", "chromosome",
        "mutation", "selection", "drift", "fitness", "ecology",
    ]),
    ("philosophy", &[
        "being", "nothing", "existence", "essence", "substance",
        "matter", "form", "force", "chaos", "beauty",
        "good", "evil", "free", "bound", "necessary", "possible",
        "universal", "particular", "abstract", "concrete",
        "subjective", "objective", "relative", "absolute",
        "finite", "infinite", "one", "many", "same", "different",
    ]),
    ("society", &[
        "person", "community", "culture", "law", "justice", "rights",
        "power", "authority", "cooperate", "compete", "share",
        "norm", "institution", "trust", "conflict", "negotiate",
        "vote", "represent", "delegate", "govern", "regulate",
    ]),
    ("economics", &[
        "price", "cost", "value", "trade", "market", "supply", "demand",
        "utility", "scarcity", "capital", "labor", "production",
        "allocate", "distribute", "optimize", "equilibrium",
        "marginal", "elastic", "efficient", "externality",
    ]),
    ("information", &[
        "signal", "noise", "entropy", "channel", "encode", "decode",
        "compress", "redundancy", "bandwidth", "latency",
        "packet", "protocol", "message", "broadcast", "multicast",
        "symmetric", "asymmetric", "key", "hash", "checksum",
    ]),
    ("measurement", &[
        "unit", "scale", "metric", "distance", "area", "volume",
        "mass", "time", "speed", "density", "pressure", "temperature",
        "angle", "frequency", "amplitude", "intensity", "flux",
        "rate", "ratio", "proportion", "percentage", "probability",
    ]),
    ("probability", &[
        "probability", "likelihood", "chance", "random", "uniform",
        "normal", "distribution", "sample", "population", "expected",
        "variance", "deviation", "correlation", "independence",
        "conditional", "prior", "posterior", "bayesian", "frequentist",
    ]),
    ("graph-theory", &[
        "vertex", "edge", "path", "cycle", "tree", "forest",
        "connected", "component", "degree", "neighbor", "adjacent",
        "directed", "undirected", "weighted", "spanning", "bipartite",
        "planar", "coloring", "clique", "matching", "flow",
    ]),
    ("number-theory", &[
        "prime", "composite", "factor", "divisor", "multiple",
        "coprime", "gcd", "lcm", "modular", "congruent",
        "residue", "quadratic", "primitive-root", "euler-totient",
        "perfect", "abundant", "deficient", "amicable",
    ]),
    ("calculus", &[
        "limit", "derivative", "integral", "gradient", "divergence",
        "curl", "laplacian", "differential", "partial", "total",
        "series", "convergent", "divergent", "taylor", "fourier",
    ]),
    ("linear-algebra", &[
        "vector", "matrix", "scalar", "transpose", "inverse",
        "determinant", "eigenvalue", "eigenvector", "trace",
        "rank", "nullity", "span", "basis", "orthogonal",
        "projection", "rotation", "reflection", "scaling",
    ]),
    ("color", &[
        "red", "green", "blue", "yellow", "cyan", "magenta",
        "white", "black", "gray", "orange", "purple", "brown",
        "hue", "saturation", "brightness", "contrast", "blend",
        "spectrum", "wavelength", "transparent", "opaque",
    ]),
    ("music", &[
        "note", "pitch", "rhythm", "tempo", "beat", "measure",
        "chord", "interval", "scale", "octave", "harmony", "melody",
        "timbre", "resonance", "overtone", "frequency",
        "major", "minor", "sharp", "flat", "natural",
    ]),
    ("emotion", &[
        "joy", "sadness", "anger", "fear", "surprise", "disgust",
        "trust", "anticipation", "love", "hate", "hope", "despair",
        "calm", "anxious", "proud", "ashamed", "curious", "bored",
        "excited", "tired", "satisfied", "frustrated",
    ]),
    ("nature", &[
        "water", "fire", "earth", "air", "light", "dark",
        "mountain", "river", "ocean", "forest", "desert", "sky",
        "sun", "moon", "star", "cloud", "rain", "wind", "snow",
        "seed", "root", "branch", "leaf", "flower", "fruit",
    ]),
    ("body", &[
        "head", "eye", "ear", "nose", "mouth", "hand", "foot",
        "heart", "brain", "lung", "bone", "muscle", "nerve",
        "blood", "breath", "pulse", "balance", "reflex",
    ]),
    ("common-verbs", &[
        "go", "come", "get", "give", "take", "put", "find", "use",
        "work", "call", "try", "ask", "need", "feel", "become",
        "leave", "show", "keep", "let", "begin", "seem", "help",
        "talk", "turn", "start", "might", "move", "live", "hold",
        "bring", "happen", "write", "read", "walk", "stand", "run",
        "build", "look", "change", "play", "meet", "lead", "grow",
    ]),
    ("common-adjectives", &[
        "good", "bad", "new", "old", "first", "last", "long", "short",
        "great", "little", "own", "right", "large", "next", "early",
        "young", "important", "few", "public", "private", "real",
        "best", "free", "strong", "whole", "able", "hard", "clear",
        "light", "heavy", "open", "close", "simple", "complex",
    ]),
    ("common-nouns", &[
        "time", "year", "people", "way", "day", "man", "child",
        "world", "life", "hand", "part", "place", "case", "week",
        "company", "system", "program", "question", "work", "government",
        "number", "night", "point", "home", "water", "room", "mother",
        "area", "money", "story", "fact", "month", "lot", "right",
        "study", "book", "eye", "job", "word", "side", "kind",
        "head", "house", "service", "friend", "father", "power",
        "hour", "game", "line", "end", "group", "problem", "state",
    ]),
    ("prepositions-conjunctions", &[
        "with", "from", "into", "through", "during", "before",
        "after", "above", "below", "between", "each", "both",
        "while", "because", "since", "until", "unless", "although",
        "whether", "without", "within", "against", "along", "among",
    ]),
    ("patterns", &[
        "recursion", "iteration", "accumulation", "transformation",
        "composition", "abstraction", "encapsulation", "polymorphism",
        "inheritance", "delegation", "observation", "notification",
        "pipeline", "filter", "map", "reduce", "fold", "unfold",
        "generator", "consumer", "producer", "stream", "lazy",
        "memoize", "cache", "retry", "backoff", "circuit-breaker",
    ]),

    // ── Everyday English — the love story ──────────────────────────────────────

    ("family-and-relationship", &[
        "mother", "father", "child", "sibling", "brother", "sister", "parent",
        "grandparent", "grandmother", "grandfather", "cousin", "aunt", "uncle",
        "family", "friend", "partner", "spouse", "husband", "wife",
        "neighbor", "stranger", "colleague", "mentor", "student",
        "bond", "kinship", "loyalty", "devotion", "affection",
        "intimacy", "companionship", "caretaker", "guardian",
        "reunion", "separation", "relationship", "connection",
        "forgiveness", "reconciliation", "commitment", "promise",
    ]),

    ("emotion-expanded", &[
        "joy", "happiness", "contentment", "pleasure", "delight", "ecstasy",
        "sadness", "sorrow", "grief", "melancholy", "longing", "nostalgia",
        "anger", "rage", "frustration", "irritation", "resentment",
        "fear", "anxiety", "dread", "terror", "panic", "worry",
        "surprise", "shock", "astonishment", "wonder", "awe",
        "disgust", "contempt", "shame", "guilt", "remorse",
        "love", "desire", "passion", "lust", "tenderness", "warmth",
        "hate", "envy", "jealousy", "bitterness", "spite",
        "pride", "vanity", "humility", "gratitude", "compassion",
        "loneliness", "isolation", "belonging", "acceptance", "rejection",
        "hope", "despair", "optimism", "pessimism", "resignation",
        "excitement", "enthusiasm", "boredom", "apathy", "restlessness",
        "calm", "serenity", "tranquility", "peace", "stillness",
        "courage", "confidence", "shyness", "embarrassment",
        "curiosity", "fascination", "wonder", "inspiration",
    ]),

    ("body-and-health", &[
        "skin", "hair", "tooth", "nail", "tongue", "throat", "shoulder",
        "arm", "elbow", "wrist", "finger", "thumb", "palm",
        "chest", "back", "stomach", "hip", "leg", "knee", "ankle", "toe",
        "spine", "rib", "skull", "jaw", "cheek", "forehead", "chin",
        "heartbeat", "pulse", "breath", "sweat", "tear", "smile", "frown",
        "sleep", "wake", "eat", "drink", "hunger", "thirst", "pain", "pleasure",
        "sick", "healthy", "heal", "recover", "wound", "scar", "fatigue",
        "strength", "weakness", "flexibility", "balance", "posture",
        "vision", "hearing", "smell", "taste", "touch", "sensation",
        "fever", "cough", "sneeze", "yawn", "stretch", "shiver",
    ]),

    ("home-and-dwelling", &[
        "house", "apartment", "room", "kitchen", "bedroom", "bathroom",
        "living-room", "hallway", "staircase", "roof", "floor", "ceiling",
        "door", "window", "wall", "corner", "shelf", "drawer", "closet",
        "table", "chair", "bed", "pillow", "blanket", "couch", "lamp",
        "mirror", "clock", "rug", "curtain", "towel", "soap",
        "home", "address", "garden", "yard", "fence", "gate", "porch",
        "neighbourhood", "street", "alley", "path", "driveway",
        "key", "lock", "bell", "mailbox", "welcome",
    ]),

    ("food-and-nourishment", &[
        "bread", "rice", "wheat", "corn", "potato", "bean", "lentil",
        "apple", "orange", "banana", "grape", "strawberry", "tomato",
        "carrot", "onion", "garlic", "pepper", "salt", "sugar", "honey",
        "milk", "butter", "cheese", "egg", "meat", "fish", "chicken",
        "soup", "stew", "salad", "sandwich", "pasta", "noodle",
        "cake", "cookie", "chocolate", "candy", "spice", "herb",
        "breakfast", "lunch", "dinner", "snack", "feast", "fast",
        "cook", "bake", "fry", "boil", "roast", "chop", "mix", "taste",
        "recipe", "ingredient", "portion", "serving", "flavour",
        "sweet", "sour", "salty", "bitter", "spicy", "savoury",
    ]),

    ("clothing-and-appearance", &[
        "shirt", "pants", "dress", "skirt", "coat", "jacket", "sweater",
        "sock", "shoe", "boot", "hat", "scarf", "glove", "belt",
        "underwear", "pyjamas", "uniform", "suit", "tie",
        "fashion", "style", "colour", "pattern", "fabric", "texture",
        "wear", "dress", "undress", "fit", "loose", "tight", "comfortable",
        "clean", "dirty", "wash", "fold", "iron", "stitch",
        "jewellery", "ring", "necklace", "earring", "bracelet", "watch",
        "appearance", "look", "beauty", "ugly", "plain", "elegant",
    ]),

    ("work-and-profession", &[
        "job", "career", "profession", "occupation", "trade", "craft", "skill",
        "office", "factory", "farm", "hospital", "school", "shop", "market",
        "boss", "employee", "manager", "worker", "teacher", "doctor",
        "engineer", "artist", "writer", "musician", "farmer", "builder",
        "task", "project", "deadline", "meeting", "report", "goal",
        "hire", "fire", "resign", "promote", "retire",
        "salary", "wage", "income", "expense", "profit", "loss",
        "productivity", "efficiency", "quality", "standard", "review",
        "collaborate", "compete", "negotiate", "delegate", "manage",
    ]),

    ("travel-and-movement", &[
        "walk", "run", "jump", "climb", "swim", "fly", "drive", "ride",
        "boat", "ship", "train", "plane", "bicycle", "car", "bus",
        "road", "highway", "bridge", "tunnel", "station", "airport",
        "city", "town", "village", "country", "continent", "world",
        "journey", "trip", "voyage", "adventure", "expedition",
        "departure", "arrival", "destination", "route", "detour",
        "passport", "ticket", "luggage", "hotel", "map",
        "north", "south", "east", "west", "direction", "compass",
        "speed", "distance", "distance", "altitude", "border",
        "migrate", "wander", "explore", "return", "settle",
    ]),

    ("communication-and-speech", &[
        "speak", "say", "tell", "ask", "answer", "explain", "describe",
        "argue", "agree", "disagree", "persuade", "convince", "negotiate",
        "conversation", "dialogue", "discussion", "debate", "lecture",
        "whisper", "shout", "sing", "hum", "laugh", "cry", "sigh",
        "letter", "message", "email", "post", "note", "diary",
        "language", "tongue", "accent", "dialect", "translation",
        "rumour", "gossip", "lie", "truth", "secret", "confession",
        "silence", "pause", "interrupt", "listen", "respond", "address",
        "greeting", "farewell", "apology", "thanks", "praise", "criticism",
        "name", "title", "nickname", "identity", "reputation", "fame",
    ]),

    ("art-and-creativity", &[
        "paint", "draw", "sketch", "sculpt", "carve", "photograph",
        "poem", "song", "novel", "play", "film", "dance",
        "brush", "canvas", "clay", "marble", "ink", "pigment",
        "colour", "line", "shape", "form", "composition", "proportion",
        "style", "technique", "genre", "theme", "symbol", "metaphor",
        "imagination", "inspiration", "vision", "expression", "creation",
        "beauty", "sublime", "harmony", "contrast", "tension", "resolution",
        "audience", "performer", "viewer", "reader", "critic", "curator",
        "gallery", "museum", "theatre", "concert", "studio",
        "draft", "revision", "craft", "mastery", "originality",
        "melody", "rhythm", "chord", "lyric", "verse", "chorus",
    ]),

    ("learning-and-education", &[
        "study", "practice", "repeat", "memorise", "understand", "apply",
        "lesson", "course", "curriculum", "subject", "topic", "theme",
        "school", "university", "library", "classroom", "laboratory",
        "teacher", "professor", "student", "pupil", "graduate",
        "book", "textbook", "notebook", "pen", "pencil", "board",
        "test", "exam", "grade", "score", "pass", "fail",
        "lecture", "seminar", "workshop", "tutorial", "assignment",
        "research", "discovery", "experiment", "observation", "theory",
        "question", "answer", "problem", "solution", "method",
        "skill", "ability", "talent", "potential", "growth",
        "curiosity", "discipline", "focus", "effort", "persistence",
    ]),

    ("nature-expanded", &[
        "forest", "jungle", "desert", "tundra", "prairie", "swamp",
        "lake", "stream", "waterfall", "glacier", "volcano", "cliff",
        "valley", "canyon", "cave", "island", "peninsula", "bay",
        "sunrise", "sunset", "dawn", "dusk", "twilight", "midnight",
        "spring", "summer", "autumn", "winter", "season", "equinox",
        "weather", "storm", "lightning", "thunder", "fog", "mist", "haze",
        "earthquake", "flood", "drought", "hurricane", "tornado",
        "animal", "bird", "fish", "insect", "reptile", "mammal",
        "wolf", "eagle", "whale", "butterfly", "ant", "bee",
        "soil", "sand", "mud", "clay", "rock", "crystal", "gem",
        "ecosystem", "habitat", "predator", "prey", "symbiosis",
        "oxygen", "carbon", "nitrogen", "hydrogen", "atmosphere",
    ]),

    ("spiritual-and-philosophical", &[
        "spirit", "soul", "divinity", "sacred", "ritual", "prayer", "meditation",
        "faith", "belief", "devotion", "worship", "blessing", "grace",
        "sin", "virtue", "redemption", "salvation", "karma",
        "meaning", "purpose", "identity", "consciousness", "awareness",
        "transcendence", "immanence", "infinite", "eternal", "void",
        "creation", "destruction", "rebirth", "cycle", "karma",
        "truth", "illusion", "maya", "enlightenment", "liberation",
        "compassion", "equanimity", "acceptance", "surrender",
        "good", "evil", "moral", "ethics", "duty", "right", "wrong",
        "free-will", "fate", "destiny", "choice", "determinism",
        "suffering", "joy", "attachment", "letting-go", "impermanence",
        "interconnection", "wholeness", "unity", "separation", "return",
    ]),

    ("social-and-political", &[
        "community", "society", "culture", "tradition", "custom", "norm",
        "law", "rule", "order", "chaos", "authority", "power",
        "government", "democracy", "republic", "constitution", "rights",
        "freedom", "justice", "equality", "opportunity", "privilege",
        "vote", "election", "citizen", "protest", "revolution",
        "war", "peace", "conflict", "cooperation", "alliance", "treaty",
        "poverty", "wealth", "class", "race", "gender", "identity",
        "immigration", "refugee", "exile", "homeland", "territory",
        "economy", "tax", "welfare", "education", "healthcare",
        "media", "propaganda", "censorship", "truth", "narrative",
        "diversity", "inclusion", "discrimination", "prejudice", "tolerance",
    ]),

    ("abstract-nouns", &[
        "love", "hate", "truth", "lie", "beauty", "ugliness",
        "good", "evil", "light", "darkness", "order", "chaos",
        "time", "eternity", "space", "void", "being", "nothingness",
        "freedom", "slavery", "justice", "injustice", "peace", "war",
        "birth", "death", "growth", "decay", "creation", "destruction",
        "joy", "suffering", "pleasure", "pain", "hope", "despair",
        "wisdom", "folly", "courage", "cowardice", "pride", "shame",
        "trust", "betrayal", "loyalty", "treachery", "honesty", "deception",
        "patience", "impatience", "generosity", "greed", "humility", "arrogance",
        "wonder", "boredom", "curiosity", "indifference", "passion", "apathy",
        "solitude", "community", "belonging", "alienation", "purpose", "meaninglessness",
        "memory", "forgetting", "identity", "transformation", "continuity", "change",
    ]),

    ("sensory-and-perception", &[
        "sight", "vision", "blind", "see", "watch", "observe", "glimpse", "stare",
        "hearing", "deaf", "listen", "hear", "sound", "noise", "music", "silence",
        "touch", "feel", "grip", "stroke", "press", "tickle", "itch",
        "taste", "flavour", "sweet", "sour", "bitter", "salty", "spicy",
        "smell", "scent", "fragrance", "odour", "perfume", "stench",
        "hot", "cold", "warm", "cool", "sharp", "dull", "smooth", "rough",
        "bright", "dim", "vivid", "faint", "loud", "quiet", "near", "distant",
        "heavy", "light", "fast", "slow", "large", "small", "wide", "narrow",
        "full", "empty", "dense", "sparse", "clear", "cloudy",
        "conscious", "dreaming", "awake", "attention", "focus", "blur",
    ]),

    ("time-and-memory", &[
        "moment", "instant", "second", "minute", "hour", "day", "week",
        "month", "year", "decade", "century", "millennium", "era", "epoch",
        "past", "present", "future", "before", "after", "during",
        "early", "late", "soon", "eventually", "never", "always",
        "yesterday", "today", "tomorrow", "recently", "once",
        "memory", "recollection", "nostalgia", "reminder", "anniversary",
        "beginning", "middle", "end", "origin", "history", "legacy",
        "rhythm", "cycle", "pattern", "repetition", "change", "evolution",
        "age", "aging", "youth", "maturity", "decline", "renewal",
        "schedule", "plan", "delay", "wait", "hurry", "pace", "tempo",
    ]),

    ("desire-and-motivation", &[
        "want", "need", "wish", "hope", "dream", "aspire", "crave", "yearn",
        "desire", "longing", "hunger", "thirst", "ambition", "goal", "aim",
        "drive", "motivation", "will", "intention", "purpose", "vision",
        "seek", "pursue", "strive", "reach", "achieve", "fulfil",
        "reward", "satisfaction", "pleasure", "gratification",
        "resist", "temptation", "impulse", "craving", "addiction",
        "enough", "content", "more", "excess", "limit", "boundary",
        "inspire", "encourage", "support", "discourage", "obstacle", "challenge",
        "risk", "dare", "attempt", "persist", "give-up", "surrender",
    ]),

    ("conflict-and-resolution", &[
        "conflict", "dispute", "argument", "quarrel", "fight", "battle",
        "attack", "defend", "resist", "yield", "retreat", "advance",
        "enemy", "opponent", "rival", "ally", "friend",
        "win", "lose", "draw", "tie", "surrender", "victory", "defeat",
        "hurt", "harm", "damage", "destroy", "heal", "repair", "restore",
        "anger", "frustration", "resentment", "forgiveness", "reconcile",
        "negotiate", "compromise", "mediate", "agree", "disagree",
        "boundary", "territory", "possession", "claim", "right",
        "power", "control", "authority", "resistance", "rebellion",
        "violence", "force", "peace", "truce", "treaty", "armistice",
    ]),

    ("narrative-and-story", &[
        "story", "tale", "narrative", "myth", "legend", "fable", "parable",
        "character", "protagonist", "antagonist", "hero", "villain",
        "plot", "setting", "theme", "conflict", "resolution", "climax",
        "beginning", "middle", "end", "chapter", "scene", "act",
        "voice", "perspective", "point-of-view", "narrator", "author",
        "describe", "show", "tell", "reveal", "hide", "foreshadow",
        "metaphor", "symbol", "irony", "tension", "suspense",
        "comedy", "tragedy", "drama", "adventure", "romance", "mystery",
        "truth", "fiction", "imagination", "reality", "world-building",
        "memory", "experience", "identity", "transformation", "journey",
    ]),

    ("common-adverbs", &[
        "very", "quite", "really", "truly", "deeply", "highly", "greatly",
        "almost", "nearly", "barely", "hardly", "scarcely",
        "always", "usually", "often", "sometimes", "rarely", "never",
        "already", "still", "yet", "soon", "now", "then", "again",
        "here", "there", "everywhere", "nowhere", "somewhere", "anywhere",
        "quickly", "slowly", "carefully", "gently", "strongly", "quietly",
        "together", "apart", "alone", "forward", "backward", "inward", "outward",
        "perhaps", "maybe", "certainly", "probably", "possibly", "definitely",
        "enough", "too", "also", "even", "just", "only", "simply",
    ]),

    ("common-prepositions", &[
        "of", "to", "in", "for", "on", "with", "at", "by", "from",
        "about", "as", "into", "through", "after", "over", "between",
        "out", "before", "within", "up", "down", "off", "under", "around",
        "along", "across", "behind", "among", "throughout", "upon",
        "beside", "beneath", "above", "below", "beyond", "near", "past",
        "via", "toward", "toward", "except", "despite", "regarding",
    ]),

    ("questions-and-discourse", &[
        "who", "what", "where", "when", "why", "how", "which",
        "whether", "whatever", "whoever", "wherever", "whenever", "however",
        "because", "therefore", "although", "whereas", "unless", "until",
        "since", "while", "after", "before", "once", "whenever",
        "if", "then", "else", "either", "neither", "both",
        "so", "thus", "hence", "consequently", "accordingly",
        "but", "yet", "however", "nevertheless", "nonetheless",
        "furthermore", "moreover", "besides", "also", "and", "or",
        "indeed", "certainly", "surely", "perhaps", "apparently",
    ]),

    ("the-body-of-language", &[
        "noun", "verb", "adjective", "adverb", "pronoun", "preposition",
        "conjunction", "article", "sentence", "clause", "phrase", "paragraph",
        "subject", "predicate", "object", "complement", "modifier",
        "singular", "plural", "tense", "past-tense", "future-tense",
        "active", "passive", "affirmative", "negative", "question",
        "synonym", "antonym", "homonym", "metaphor", "simile", "idiom",
        "prefix", "suffix", "root", "stem", "syllable", "stress", "rhythm",
        "punctuation", "comma", "period", "colon", "semicolon",
        "capital", "lowercase", "indent", "margin", "spacing",
        "vocabulary", "grammar", "syntax", "semantics", "pragmatics",
    ]),

    ("common-verbs-expanded", &[
        "love", "hate", "want", "need", "feel", "believe", "know", "think",
        "remember", "forget", "hope", "fear", "trust", "doubt",
        "accept", "reject", "allow", "prevent", "protect", "destroy",
        "begin", "end", "continue", "pause", "stop", "start",
        "approach", "avoid", "follow", "lead", "guide", "teach",
        "give", "take", "share", "keep", "lose", "find", "search",
        "open", "close", "enter", "leave", "arrive", "depart",
        "speak", "listen", "ask", "answer", "agree", "disagree",
        "help", "hurt", "heal", "care", "neglect", "ignore",
        "grow", "shrink", "expand", "contract", "rise", "fall",
        "connect", "separate", "join", "break", "merge", "split",
        "create", "make", "build", "design", "invent", "discover",
        "observe", "measure", "test", "verify", "prove", "disprove",
    ]),

    ("numbers-and-counting", &[
        "first", "second", "third", "fourth", "fifth", "sixth", "seventh",
        "eighth", "ninth", "tenth", "eleventh", "twelfth",
        "once", "twice", "thrice", "many", "few", "several", "couple",
        "single", "double", "triple", "quadruple", "multiple",
        "half", "quarter", "third-part", "whole", "fraction", "percentage",
        "count", "tally", "total", "sum", "average", "median", "maximum", "minimum",
        "increase", "decrease", "multiply", "divide", "add", "subtract",
        "positive", "negative", "zero", "infinite", "finite",
        "approximate", "exact", "round", "estimate", "calculate",
    ]),

    ("light-and-darkness", &[
        "light", "glow", "shine", "sparkle", "gleam", "glitter", "flash",
        "radiance", "luminous", "bright", "brilliant", "vivid", "dazzling",
        "dark", "shadow", "shade", "dim", "dusk", "gloom", "obscure",
        "transparent", "opaque", "translucent", "clear", "cloudy",
        "colour", "spectrum", "rainbow", "prismatic", "hue", "saturation",
        "red", "orange", "yellow", "green", "blue", "purple", "violet",
        "white", "black", "grey", "brown", "pink", "gold", "silver",
        "mirror", "reflection", "refraction", "shadow", "eclipse",
        "dawn", "noon", "twilight", "midnight", "sunrise", "sunset",
        "candle", "lamp", "lantern", "star", "moon", "sun", "fire",
    ]),

    ("connection-and-belonging", &[
        "belong", "include", "exclude", "accept", "welcome", "reject", "cast-out",
        "bond", "attach", "tie", "link", "connect", "weave", "knit",
        "community", "tribe", "clan", "nation", "species", "kind",
        "home", "origin", "root", "ancestry", "heritage", "tradition",
        "identity", "name", "role", "place", "position", "status",
        "equal", "different", "similar", "related", "distant", "close",
        "familiar", "unknown", "trusted", "feared", "loved", "overlooked",
        "seen", "heard", "understood", "valued", "respected", "dismissed",
        "alone", "together", "isolated", "integrated", "estranged", "reconciled",
    ]),
];

// ── System prompt ─────────────────────────────────────────────────────────────

pub const GENERATION_SYSTEM_PROMPT: &str = "\
You are a Forth code generator building the Co-Forth English dictionary.\n\
Each English word becomes a Forth snippet — its computational identity.\n\
\n\
AVAILABLE WORDS (use ONLY these):\n\
  Builtins: + - * / mod  dup drop swap over rot nip tuck  2dup 2drop 2swap\n\
    = < > <= >= <> 0= 0< 0>  and or xor invert negate abs max min\n\
    lshift rshift  . .\" cr space emit  @ ! +!  i j  >r r> r@\n\
    pick roll  */ /mod  u. .h  depth  nop  random  time\n\
  Stdlib: square cube  2* 2/ 1+ 1-  within  sum-to-n  gcd lcm  pow  fib\n\
    sign  even? odd?  positive? negative? zero?  bool  true false\n\
    between clamp  -rot  ?dup  tally  .bin8  digits  iota-sum\n\
    bit set-bit clr-bit tst-bit  nl .cr spaces tab banner .bool\n\
\n\
SAFETY RULES (ALL mandatory):\n\
  1. Every loop MUST terminate: do/loop with literal integer bounds,\n\
     begin/until where the exit condition is guaranteed.\n\
  2. Stack depth NEVER exceeds 8 items.\n\
  3. Stack is clean at end (depth 0).\n\
  4. MUST produce at least one output character.\n\
  5. Snippet is 1-4 tokens — concise and illustrative.\n\
  6. No variable declarations (only top-level Forth can declare variables).\n\
\n\
The Forth should DEMONSTRATE the word's meaning computationally.\n\
\n\
Return a JSON array, one object per input word:\n\
[\n\
  {\n\
    \"word\": \"example\",\n\
    \"definition\": \"a particular instance illustrating a general rule\",\n\
    \"related\": [\"instance\", \"case\", \"rule\", \"demonstrate\"],\n\
    \"kind\": \"observation\",\n\
    \"forth\": \"5 square . .\\\" = 5^2\\\" cr\"\n\
  }\n\
]\n\
Return ONLY the JSON array. No markdown fences, no explanation.";

// ── TOML serialization ────────────────────────────────────────────────────────

pub fn entry_to_toml(word: &str, definition: &str, related: &[String], kind: &str, forth: &str) -> String {
    let related_str = related.iter()
        .take(4)
        .map(|r| format!("\"{}\"", r.to_lowercase()))
        .collect::<Vec<_>>()
        .join(", ");

    // Use TOML literal string (single quotes) when possible — no escaping needed
    let forth_toml = if !forth.contains('\'') {
        format!("'{forth}'")
    } else {
        let escaped = forth.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    };

    format!(
        "[[word]]\nword = \"{word}\"\ndefinition = \"{def}\"\nrelated = [{related_str}]\nkind = \"{kind}\"\nforth = {forth_toml}\n\n",
        word = word,
        def = definition.replace('"', "\\\""),
        related_str = related_str,
        kind = kind,
        forth_toml = forth_toml,
    )
}

// ── Load existing words from a TOML file ──────────────────────────────────────

pub fn existing_words(path: &Path) -> HashSet<String> {
    let mut set = HashSet::new();
    if let Ok(content) = std::fs::read_to_string(path) {
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("word = \"") {
                if let Some(name) = rest.strip_suffix('"') {
                    set.insert(name.to_string());
                }
            }
        }
    }
    set
}

// ── Output path helpers ───────────────────────────────────────────────────────

/// Default output: user library (loaded at runtime, no rebuild needed).
pub fn user_library_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".finch")
        .join("library.toml")
}

/// Embedded path: bake into the binary (requires `cargo build` after writing).
pub fn embedded_library_path() -> PathBuf {
    // Relative to the workspace root
    PathBuf::from("src/coforth/english_library.toml")
}

// ── Generator ─────────────────────────────────────────────────────────────────

/// Options for `finch library build`.
pub struct BuildOptions {
    pub category: Option<String>,
    pub words: Option<Vec<String>>,
    pub all: bool,
    pub batch_size: usize,
    pub validate: bool,
    pub output: PathBuf,
}

/// Generate Forth for a flat list of words, streaming results to `output`.
///
/// Uses the configured provider (must be Claude-compatible).
pub async fn build_library(
    opts: BuildOptions,
    generator: std::sync::Arc<dyn crate::generators::Generator>,
) -> Result<(usize, usize)> {
    // Collect the word list
    let all_words: Vec<&str> = match (opts.all, opts.category.as_deref(), opts.words.as_deref()) {
        (true, _, _) => {
            CATEGORIES.iter().flat_map(|(_, words)| words.iter().copied()).collect()
        }
        (_, Some(cat), _) => {
            CATEGORIES.iter()
                .find(|(name, _)| *name == cat)
                .map(|(_, words)| words.iter().copied().collect())
                .with_context(|| format!("unknown category '{cat}'"))?
        }
        (_, _, Some(words)) => words.iter().map(|s| s.as_str()).collect(),
        _ => anyhow::bail!("specify --all, --category, or --words"),
    };

    // Deduplicate and skip already-generated words
    let existing = existing_words(&opts.output);
    let mut seen = HashSet::new();
    seen.extend(existing);

    let pending: Vec<&str> = all_words.into_iter()
        .filter(|w| seen.insert(w.to_string()))
        .collect();

    if pending.is_empty() {
        println!("All words already generated.");
        return Ok((0, 0));
    }

    println!("Generating {} words → {}", pending.len(), opts.output.display());
    println!();

    // Ensure output file exists with header
    if !opts.output.exists() {
        if let Some(parent) = opts.output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&opts.output,
            "# Co-Forth English Library\n\
             # Generated by `finch library build --all`\n\
             # Re-generate: finch library build --all\n\n"
        )?;
    }

    let total_batches = (pending.len() + opts.batch_size - 1) / opts.batch_size;
    println!("{total_batches} batches × {} words each — running in parallel", opts.batch_size);

    // Shared file mutex for concurrent writes
    let file_mutex = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    let output_path = std::sync::Arc::new(opts.output.clone());
    let validate = opts.validate;

    // Spawn all batches concurrently
    let handles: Vec<_> = pending
        .chunks(opts.batch_size)
        .enumerate()
        .map(|(batch_idx, chunk)| {
            let words: Vec<String> = chunk.iter().map(|s| s.to_string()).collect();
            let gen = std::sync::Arc::clone(&generator);
            let file_mutex = std::sync::Arc::clone(&file_mutex);
            let out_path = std::sync::Arc::clone(&output_path);

            tokio::spawn(async move {
                let word_list = words.join(", ");
                println!("[batch {batch_idx}] {word_list}");

                let messages = vec![
                    crate::claude::Message {
                        role: "user".to_string(),
                        content: vec![crate::claude::ContentBlock::Text {
                            text: GENERATION_SYSTEM_PROMPT.to_string(),
                        }],
                    },
                    crate::claude::Message {
                        role: "assistant".to_string(),
                        content: vec![crate::claude::ContentBlock::Text {
                            text: "Understood. Returning only a JSON array.".to_string(),
                        }],
                    },
                    crate::claude::Message {
                        role: "user".to_string(),
                        content: vec![crate::claude::ContentBlock::Text {
                            text: format!("Generate Co-Forth entries for: {word_list}"),
                        }],
                    },
                ];

                let response = match gen.generate(messages, None).await {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("[batch {batch_idx}] API error: {e}");
                        return (0usize, words.len());
                    }
                };

                let text = response.text.trim().to_string();
                let text = strip_markdown_fences(&text).to_string();

                let entries: Vec<serde_json::Value> = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("[batch {batch_idx}] JSON error: {e}");
                        return (0, words.len());
                    }
                };

                let mut ok = 0usize;
                let mut fail = 0usize;
                let mut toml_buf = String::new();

                for entry in &entries {
                    let word = entry["word"].as_str().unwrap_or("").to_lowercase();
                    let definition = entry["definition"].as_str().unwrap_or("");
                    let kind = entry["kind"].as_str().unwrap_or("observation");
                    let forth = entry["forth"].as_str().unwrap_or("");
                    let related: Vec<String> = entry["related"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                        .unwrap_or_default();

                    if word.is_empty() || forth.is_empty() { fail += 1; continue; }

                    if validate {
                        match crate::coforth::Forth::run(forth) {
                            Ok(out) if !out.is_empty() => {
                                println!("  ✓ {word}");
                            }
                            Ok(_) => { eprintln!("  ✗ {word}: no output"); fail += 1; continue; }
                            Err(e) => { eprintln!("  ✗ {word}: {e}"); fail += 1; continue; }
                        }
                    } else {
                        println!("  + {word}");
                    }

                    toml_buf.push_str(&entry_to_toml(&word, definition, &related, kind, forth));
                    ok += 1;
                }

                // Write batch results to file under lock
                if !toml_buf.is_empty() {
                    let _guard = file_mutex.lock().await;
                    if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(out_path.as_ref()) {
                        let _ = file.write_all(toml_buf.as_bytes());
                    }
                }

                (ok, fail)
            })
        })
        .collect();

    // Collect results
    let mut ok_count = 0usize;
    let mut fail_count = 0usize;
    for handle in handles {
        match handle.await {
            Ok((ok, fail)) => { ok_count += ok; fail_count += fail; }
            Err(e) => { eprintln!("batch task panicked: {e}"); }
        }
    }

    println!();
    println!("Done: {ok_count} written, {fail_count} failed → {}", opts.output.display());
    Ok((ok_count, fail_count))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn strip_markdown_fences(s: &str) -> &str {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix("```json") {
        return inner.trim_end_matches("```").trim();
    }
    if let Some(inner) = s.strip_prefix("```") {
        return inner.trim_end_matches("```").trim();
    }
    s
}

#[allow(dead_code)]
fn truncate(s: &str, max: usize) -> &str {
    let s = s.trim();
    if s.len() <= max { s } else { &s[..max] }
}
