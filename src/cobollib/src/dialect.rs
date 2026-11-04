//! Per-dialect table for the COBOL frontend (`cobol.dialect`, `-std=<dialect>`).
//! Written from the GnuCOBOL manual (SPEC §19), never copied from GnuCOBOL's GPL config.

/// A COBOL dialect. `cobol2023` is refused by [`Dialect::from_name`] until P7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Cobol85,
    Cobol2002,
    Cobol2014,
    Ibm,
}

/// Source layout. `Cobol85` is fixed layout with area A checks (SPEC §3, §19.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    Fixed,
    Cobol85,
}

/// Length limits in characters, per dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub word: usize,
    pub literal: usize,
    pub pic: usize,
}

/// Feature levels, as GnuCOBOL names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureLevel {
    Error,
    Unconformable,
    Warning,
    Archaic,
    Obsolete,
    Ok,
    Ignore,
    Skip,
}

impl FeatureLevel {
    /// `error`, `unconformable`, `warning`, `archaic` and `obsolete` give a diagnostic.
    pub fn emits_diagnostic(self) -> bool {
        !matches!(self, Self::Ok | Self::Ignore | Self::Skip)
    }

    /// `error` and `unconformable` (same as error) skip the construct; the rest still scan.
    pub fn skips_construct(self) -> bool {
        matches!(self, Self::Error | Self::Unconformable)
    }
}

/// Constructs with a starting feature level (SPEC §19.5). Further constructs join as
/// the parser needs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Alter,
    GoToWithoutName,
    Entry,
    SectionSegments,
}

/// OO words: contextual keywords under every dialect (SPEC §8). Only `CLASS` is reserved,
/// and only under `cobol85` (see [`Dialect::is_reserved`]).
pub const OO_WORDS: [&str; 13] = [
    "CLASS",
    "OBJECT",
    "METHOD",
    "FACTORY",
    "INTERFACE",
    "SELF",
    "SUPER",
    "NEW",
    "NULL",
    "PROPERTY",
    "GET",
    "INHERITS",
    "IMPLEMENTS",
];

/// Whether `word` is one of the OO words (case-insensitive).
pub fn is_oo_word(word: &str) -> bool {
    OO_WORDS.iter().any(|w| w.eq_ignore_ascii_case(word))
}

impl Dialect {
    /// The dialect used when none is configured.
    pub const DEFAULT: Dialect = Dialect::Cobol85;

    /// Parses the `-std=<dialect>` value. Refusals carry the message the user sees.
    pub fn from_name(name: &str) -> Result<Dialect, String> {
        match name {
            "cobol85" => Ok(Dialect::Cobol85),
            "cobol2002" => Ok(Dialect::Cobol2002),
            "cobol2014" => Ok(Dialect::Cobol2014),
            "ibm" => Ok(Dialect::Ibm),
            "cobol2023" => Err(
                "dialect cobol2023 is a placeholder with no features yet; use cobol85, cobol2002, cobol2014 or ibm"
                    .to_string(),
            ),
            other => Err(format!(
                "unknown dialect `{other}`; expected cobol85, cobol2002, cobol2014 or ibm"
            )),
        }
    }

    /// OO recognition is on under every dialect except `cobol85`.
    pub fn oo_enabled(self) -> bool {
        self != Dialect::Cobol85
    }

    /// `cobol85` and `ibm` use the `cobol85` format; `cobol2002` and `cobol2014` use fixed.
    pub fn default_format(self) -> SourceFormat {
        match self {
            Dialect::Cobol85 | Dialect::Ibm => SourceFormat::Cobol85,
            Dialect::Cobol2002 | Dialect::Cobol2014 => SourceFormat::Fixed,
        }
    }

    pub fn limits(self) -> Limits {
        let (word, literal, pic) = match self {
            Dialect::Cobol85 => (30, 160, 30),
            Dialect::Cobol2002 => (31, 160, 50),
            Dialect::Cobol2014 => (31, 8191, 63),
            Dialect::Ibm => (30, 160, 50),
        };
        Limits { word, literal, pic }
    }

    /// Reserved words (case-insensitive). Only the words SPEC §19.4 names are encoded:
    /// `cobol85` reserves CLASS and ALTER, `ibm` reserves ENTRY and ALTER. No full
    /// per-dialect list is sourced yet, so `cobol2002`/`cobol2014` reserve nothing here.
    pub fn is_reserved(self, word: &str) -> bool {
        let set: &[&str] = match self {
            Dialect::Cobol85 => &["CLASS", "ALTER"],
            Dialect::Ibm => &["ENTRY", "ALTER"],
            Dialect::Cobol2002 | Dialect::Cobol2014 => &[],
        };
        set.iter().any(|w| w.eq_ignore_ascii_case(word))
    }

    /// Feature level of a construct. The starting entries of SPEC §19.5 apply under every
    /// dialect until per-dialect data is sourced.
    pub fn feature_level(self, feature: Feature) -> FeatureLevel {
        match feature {
            Feature::Alter | Feature::GoToWithoutName => FeatureLevel::Obsolete,
            Feature::Entry => FeatureLevel::Ok,
            Feature::SectionSegments => FeatureLevel::Ignore,
        }
    }
}
