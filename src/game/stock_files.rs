//! The stock `.big` archives shipped with each game. Any other archive found
//! in the game folder is a leftover mod file and gets shadowed before launch.
//! Names are lower-cased for case-insensitive lookup, as in `InitWindow`.

pub const ZERO_HOUR_FILES: &[&str] = &[
    "audiochinesezh.big",
    "gensec.big",
    "audioenglishzh.big",
    "audiofrenchzh.big",
    "audiogermanzh.big",
    "audioitalianzh.big",
    "audiokoreanzh.big",
    "audiopolishzh.big",
    "audiospanishzh.big",
    "audiozh.big",
    "brazilianzh.big",
    "chinesezh.big",
    "englishzh.big",
    "frenchzh.big",
    "germanzh.big",
    "italianzh.big",
    "koreanzh.big",
    "polishzh.big",
    "spanishzh.big",
    "genseczh.big",
    "inizh.big",
    "mapszh.big",
    "music.big",
    "musiczh.big",
    "patchzh.big",
    "shaderszh.big",
    "speechbrazilianzh.big",
    "speechchinesezh.big",
    "speechenglishzh.big",
    "speechfrenchzh.big",
    "speechgermanzh.big",
    "speechitalianzh.big",
    "speechkoreanzh.big",
    "speechpolishzh.big",
    "speechspanishzh.big",
    "speechzh.big",
    "terrainzh.big",
    "textureszh.big",
    "w3denglishzh.big",
    "w3dgermanzh.big",
    "w3dchinesezh.big",
    "w3dgerman2zh.big",
    "w3ditalianzh.big",
    "w3dkoreanzh.big",
    "w3dpolishzh.big",
    "w3dspanishzh.big",
    "w3dzh.big",
    "windowzh.big",
    "patchdata.big",
    "patchini.big",
    "patchwindow.big",
];

pub const GENERALS_FILES: &[&str] = &[
    "audio.big",
    "audiobrazilian.big",
    "audiochinese.big",
    "audioenglish.big",
    "audiofrench.big",
    "audiogerman.big",
    "audiogerman2.big",
    "audioitalian.big",
    "audiokorean.big",
    "audiopolish.big",
    "audiospanish.big",
    "brazilian.big",
    "chinese.big",
    "english.big",
    "french.big",
    "german.big",
    "german2.big",
    "italian.big",
    "korean.big",
    "polish.big",
    "spanish.big",
    "gensec.big",
    "ini.big",
    "maps.big",
    "music.big",
    "patch.big",
    "shaders.big",
    "speech.big",
    "speechbrazilian.big",
    "speechchinese.big",
    "speechenglish.big",
    "speechfrench.big",
    "speechgerman.big",
    "speechgerman2.big",
    "speechitalian.big",
    "speechkorean.big",
    "speechpolish.big",
    "speechspanish.big",
    "w3dchinese.big",
    "w3dgerman2.big",
    "w3ditalian.big",
    "w3dkorean.big",
    "w3dpolish.big",
    "w3dspanish.big",
    "terrain.big",
    "textures.big",
    "w3d.big",
    "window.big",
    "patchdata.big",
    "patchini.big",
    "patchwindow.big",
];

use crate::config::Game;
use std::collections::HashSet;

pub fn for_game(game: Game) -> HashSet<String> {
    let list = match game {
        Game::ZeroHour => ZERO_HOUR_FILES,
        Game::Generals => GENERALS_FILES,
    };
    list.iter().map(|s| (*s).to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_lists_are_populated_and_lower_cased() {
        let zh = for_game(Game::ZeroHour);
        assert!(zh.contains("windowzh.big"));
        assert!(zh.len() > 40);
        let gen = for_game(Game::Generals);
        assert!(gen.contains("window.big"));
        assert!(gen.len() > 40);
    }
}
