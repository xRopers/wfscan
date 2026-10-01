//! What a file's type number means.
//!
//! The common header's type is only a number, and the same number means different things
//! in different games and package categories (163 is a texture in `Texture`, a lightmap
//! in `LightMap` and a Soulframe terrain model in `Misc`). This table is the one place
//! that says which numbers belong to which kind of asset. A new number after a game
//! update is one line here; `wfscan report` lists the numbers that have no line.
//!
//! Source: the type lists Warframe-Exporter registers per game and package category, as
//! of its June 2026 master. Lines marked "seen" were added from what a real cache holds.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::cache::package::{Category, Game};

/// A kind of asset wfscan knows about, each with (eventually) one exporter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Model,
    Texture,
    Material,
    Audio,
    Level,
    LevelStatic,
    Landscape,
    Shader,
    Icon,
}

impl Kind {
    pub const ALL: [Kind; 9] = [
        Kind::Model,
        Kind::Texture,
        Kind::Material,
        Kind::Audio,
        Kind::Level,
        Kind::LevelStatic,
        Kind::Landscape,
        Kind::Shader,
        Kind::Icon,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Model => "model",
            Kind::Texture => "texture",
            Kind::Material => "material",
            Kind::Audio => "audio",
            Kind::Level => "level",
            Kind::LevelStatic => "level-static",
            Kind::Landscape => "landscape",
            Kind::Shader => "shader",
            Kind::Icon => "icon",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Kind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Kind::ALL.into_iter().find(|k| k.name() == s).ok_or_else(|| {
            let names: Vec<&str> = Kind::ALL.iter().map(|k| k.name()).collect();
            format!("unknown kind {s:?} (one of: {})", names.join(", "))
        })
    }
}

/// Which games a line applies to.
const W: u8 = 1;
const S: u8 = 2;
const WS: u8 = W | S;

struct Rule {
    games: u8,
    category: Category,
    kind: Kind,
    types: &'static [u32],
}

const fn rule(games: u8, category: Category, kind: Kind, types: &'static [u32]) -> Rule {
    Rule { games, category, kind, types }
}

#[rustfmt::skip]
const RULES: &[Rule] = &[
    // Models. Several numbers are shared by the plain, `_dcm`, `_hlod` and `_sm` layouts,
    // which the model reader tells apart by file name. Warframe's 96 is left out: since
    // update 43 dialogue files under /Lotus/Types/Gameplay/1999Wf/Dialogue use it too.
    rule(W,  Category::Misc, Kind::Model, &[86, 99, 101, 102, 103, 105, 108, 109]),
    rule(S,  Category::Misc, Kind::Model, &[102, 105, 106, 108, 109, 111, 112, 113]),
    rule(W,  Category::Misc, Kind::Model, &[159, 160, 166]),                    // terrain
    rule(S,  Category::Misc, Kind::Model, &[163, 166, 169, 170]),
    rule(W,  Category::Misc, Kind::Model, &[269, 272, 281]),                    // rigged
    rule(S,  Category::Misc, Kind::Model, &[275, 277, 278, 283, 287, 288]),
    rule(W,  Category::Misc, Kind::Model, &[289, 290, 299]),                    // packed
    rule(S,  Category::Misc, Kind::Model, &[300, 301, 310, 314, 315, 316]),

    rule(WS, Category::Texture,  Kind::Texture, &[163, 164, 165, 166, 167, 171, 174, 176, 177, 184, 188, 194, 195]),
    rule(S,  Category::Texture,  Kind::Texture, &[175]),
    rule(WS, Category::LightMap, Kind::Texture, &[163, 164, 166, 175, 176, 177, 181]),
    // Seen: 12,404 `_cross.hdr` files in the September 2026 cache, every one of which
    // reads as a BC6H cube map. Probably what 181 (HDR) became.
    rule(W,  Category::LightMap, Kind::Texture, &[183]),

    rule(W,  Category::Misc, Kind::Material, &[203, 204, 205, 206, 208, 209, 214, 216, 217]),
    rule(S,  Category::Misc, Kind::Material, &[208, 209, 216, 217]),
    // Seen: 38,658 files in the September 2026 cache, all named like materials. The
    // original lists 210 among its material types without registering it.
    rule(W,  Category::Misc, Kind::Material, &[210]),

    // One type; the audio header says whether the data is Opus, PCM or ADPCM.
    rule(W,  Category::Misc, Kind::Audio, &[139]),

    rule(W,  Category::AnimRetarget, Kind::Level, &[201, 202, 207, 209, 212]),
    rule(S,  Category::AnimRetarget, Kind::Level, &[202, 212]),
    rule(W,  Category::AnimRetarget, Kind::LevelStatic, &[28, 29, 30]),
    rule(S,  Category::AnimRetarget, Kind::LevelStatic, &[34]),

    rule(W,  Category::Misc, Kind::Landscape, &[42]),
    rule(S,  Category::Misc, Kind::Landscape, &[42, 43]),

    rule(W,  Category::ShaderPermutation, Kind::Shader, &[18, 23, 24, 25, 26, 29]),
    // Seen: 12,079 files in the September 2026 cache. The original has an ImHex pattern
    // for shader type 32 but doesn't register it.
    rule(W,  Category::ShaderPermutation, Kind::Shader, &[32]),

    rule(W,  Category::Misc, Kind::Icon, &[3]),
];

/// The kind of asset a type number stands for, if wfscan knows.
pub fn kind_of(game: Game, category: Category, type_id: u32) -> Option<Kind> {
    let game = match game {
        Game::Warframe | Game::WarframePreEnsmallening => W,
        Game::Soulframe => S,
        Game::Unknown => return None,
    };
    RULES.iter().find(|r| r.games & game != 0 && r.category == category && r.types.contains(&type_id)).map(|r| r.kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_number_differs_by_game_and_category() {
        assert_eq!(kind_of(Game::Warframe, Category::Texture, 163), Some(Kind::Texture));
        assert_eq!(kind_of(Game::Warframe, Category::LightMap, 163), Some(Kind::Texture));
        assert_eq!(kind_of(Game::Warframe, Category::Misc, 163), None);
        assert_eq!(kind_of(Game::Soulframe, Category::Misc, 163), Some(Kind::Model));
        assert_eq!(kind_of(Game::Warframe, Category::Misc, 281), Some(Kind::Model));
        assert_eq!(kind_of(Game::Soulframe, Category::Misc, 281), None);
        assert_eq!(kind_of(Game::Warframe, Category::Misc, 96), None, "clashes with dialogue files");
        assert_eq!(kind_of(Game::Warframe, Category::AnimRetarget, 28), Some(Kind::LevelStatic));
        assert_eq!(kind_of(Game::Unknown, Category::Texture, 163), None);
    }

    #[test]
    fn no_number_has_two_kinds() {
        for (i, a) in RULES.iter().enumerate() {
            for b in &RULES[i + 1..] {
                if a.games & b.games != 0 && a.category == b.category {
                    let shared: Vec<_> = a.types.iter().filter(|t| b.types.contains(t)).collect();
                    assert!(shared.is_empty(), "{:?} {shared:?} is both {} and {}", a.category, a.kind, b.kind);
                }
            }
        }
    }

    #[test]
    fn kind_names_parse_back() {
        for kind in Kind::ALL {
            assert_eq!(kind.name().parse::<Kind>(), Ok(kind));
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{}\"", kind.name()));
        }
        assert!("mesh".parse::<Kind>().unwrap_err().contains("level-static"));
    }
}
