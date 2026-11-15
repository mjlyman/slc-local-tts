//! What the Voices page shows: one section per engine, one row per
//! download, each with its state and the buttons that state allows, and the
//! question Delete asks first.

use crate::api::slc::dto::ModelDto;
use crate::catalog::Engine;
use crate::models::{FailureKind, ModelState};
use crate::names::{format_mb, row_title};

/// Which rows the page lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Installed,
    /// Everything not installed, downloads in progress and failed ones
    /// included.
    NotInstalled,
}

impl Filter {
    /// Every filter, in the order the page offers them.
    pub const EVERY: [Filter; 3] = [Filter::All, Filter::Installed, Filter::NotInstalled];

    /// The filter's name on the page.
    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Installed => "Installed",
            Filter::NotInstalled => "Not installed",
        }
    }

    fn keeps(self, state: &ModelState) -> bool {
        let installed = matches!(state, ModelState::Installed);
        match self {
            Filter::All => true,
            Filter::Installed => installed,
            Filter::NotInstalled => !installed,
        }
    }
}

/// One engine's rows, under its name and a line about it.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub engine: Engine,
    pub title: &'static str,
    pub description: &'static str,
    pub rows: Vec<Row>,
}

/// One download: a model and the voices it holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// The model's name, which Download and Delete take.
    pub stem: String,
    pub title: String,
    /// The voice count and size, such as "28 voices · 350 MB · slow".
    pub subline: String,
    /// "Installed", "161 of 350 MB", "Not installed", or why it failed.
    pub status_text: String,
    /// True when the status text reports a failed download.
    pub problem: bool,
    /// How far the download has got, 0 to 1, while it runs.
    pub progress: Option<f32>,
    pub can_play: bool,
    pub can_download: bool,
    pub can_delete: bool,
    /// The voices in the model, as (ID, name).
    pub voices: Vec<(String, String)>,
}

/// The engines in the order the page lists them, with each section's name
/// (which is also the engine's name in a sentence) and line. Every other
/// list of engines here is worked out from this one.
const SECTIONS: [(Engine, &str, &str); 3] = [
    (
        Engine::Supertonic,
        "Supertonic",
        "Ten natural voices in one download. Quick on most PCs.",
    ),
    (
        Engine::Kokoro,
        "Kokoro",
        "28 expressive voices in one download. Slow on long announcements.",
    ),
    (
        Engine::Piper,
        "Piper",
        "One download per voice. The fastest voices, good for long announcements.",
    ),
];

/// The engine a model's `engine` field names, if it's one this version
/// knows.
fn engine_of(model: &ModelDto) -> Option<Engine> {
    SECTIONS
        .iter()
        .map(|&(engine, _, _)| engine)
        .find(|engine| engine.as_str() == model.engine)
}

/// The engine's name in a sentence, such as "Kokoro".
fn engine_name(engine: Engine) -> &'static str {
    SECTIONS
        .iter()
        .find(|&&(listed, _, _)| listed == engine)
        .map_or(engine.as_str(), |&(_, title, _)| title)
}

/// The page's sections for `models`, keeping the rows that match `search`
/// (ignoring case, in the row's title or any voice's ID or name) and
/// `filter`. A section with no rows left is left out.
pub fn sections(models: &[ModelDto], search: &str, filter: Filter) -> Vec<Section> {
    let search = search.trim().to_lowercase();
    SECTIONS
        .iter()
        .map(|&(engine, title, description)| Section {
            engine,
            title,
            description,
            rows: models
                .iter()
                .filter(|model| engine_of(model) == Some(engine))
                .filter(|model| filter.keeps(&model.state))
                .map(|model| row(model, engine))
                .filter(|row| matches(row, &search))
                .collect(),
        })
        .filter(|section| !section.rows.is_empty())
        .collect()
}

/// True if `row` matches `search`, which is already lowercase. An empty
/// search matches every row.
fn matches(row: &Row, search: &str) -> bool {
    if search.is_empty() {
        return true;
    }
    let has = |text: &str| text.to_lowercase().contains(search);
    has(&row.title) || row.voices.iter().any(|(id, name)| has(id) || has(name))
}

fn row(model: &ModelDto, engine: Engine) -> Row {
    let mb = format_mb(model.size);
    let count = model.voices.len();
    let mut subline = if count == 1 {
        format!("{mb} MB")
    } else {
        format!("{count} voices · {mb} MB")
    };
    if engine == Engine::Kokoro {
        subline.push_str(" · slow");
    }

    let (status_text, progress) = match &model.state {
        ModelState::Installed => ("Installed".to_string(), None),
        ModelState::NotInstalled => ("Not installed".to_string(), None),
        // A finished download is still being unpacked and checked.
        ModelState::Downloading { done, total } if done >= total => {
            ("Unpacking…".to_string(), Some(1.0))
        }
        ModelState::Downloading { done, total } => (
            format!("{} of {} MB", format_mb(*done), format_mb(*total)),
            Some(*done as f32 / *total as f32),
        ),
        ModelState::Failed { reason, kind } => (failure_text(reason, *kind), None),
    };

    let installed = matches!(model.state, ModelState::Installed);
    let can_download = match &model.state {
        ModelState::NotInstalled => true,
        // Only an app update fixes a permanent failure, so trying again
        // would fail the same way.
        ModelState::Failed { kind, .. } => *kind != FailureKind::Permanent,
        ModelState::Installed | ModelState::Downloading { .. } => false,
    };
    Row {
        stem: model.stem.clone(),
        title: row_title(&model.stem),
        subline,
        status_text,
        problem: matches!(model.state, ModelState::Failed { .. }),
        progress,
        can_play: installed,
        can_download,
        can_delete: installed,
        voices: model
            .voices
            .iter()
            .map(|voice| (voice.id.clone(), voice.name.clone()))
            .collect(),
    }
}

/// The voice `row`'s Play button plays: `picked` while the row still lists
/// it, else the row's first voice. `None` for a row with no voices.
pub fn play_voice(row: &Row, picked: Option<&str>) -> Option<String> {
    let listed = |id: &str| row.voices.iter().any(|(voice, _)| voice == id);
    match picked {
        Some(id) if listed(id) => Some(id.to_string()),
        _ => row.voices.first().map(|(id, _)| id.clone()),
    }
}

/// The first voice in a row of many voices that `search` names by ID or
/// name, ignoring case, so its picker can start on what was searched for.
/// `None` for a blank search, a one-voice row, or no match.
pub fn search_pick(row: &Row, search: &str) -> Option<String> {
    let search = search.trim().to_lowercase();
    if search.is_empty() || row.voices.len() < 2 {
        return None;
    }
    let has = |text: &str| text.to_lowercase().contains(&search);
    row.voices
        .iter()
        .find(|(id, name)| has(id) || has(name))
        .map(|(id, _)| id.clone())
}

/// The model Delete's question is about, while there's still something to
/// delete: `None` once `stem` has gone from `models` or is no longer
/// installed (another copy deleted it, say), which closes the question.
pub fn delete_target<'a>(models: &'a [ModelDto], stem: &str) -> Option<&'a ModelDto> {
    models
        .iter()
        .find(|model| model.stem == stem && matches!(model.state, ModelState::Installed))
}

/// What a failed row says: the reason, and what to do about it.
fn failure_text(reason: &str, kind: FailureKind) -> String {
    match kind {
        FailureKind::Network => {
            format!(
                "Couldn't download: {reason}. Check your connection, then press Download again."
            )
        }
        FailureKind::Permanent => format!("Couldn't download: {reason}."),
        FailureKind::Other => {
            format!("Couldn't download: {reason}. Press Download to try again.")
        }
    }
}

/// The question Delete asks before it deletes `model`, saying which voices
/// go with the files and what SLC does without them.
pub fn delete_prompt(model: &ModelDto) -> String {
    let title = row_title(&model.stem);
    let mb = format_mb(model.size);
    let count = model.voices.len();
    let after = "the next time SLC asks for it, and those announcements get SLC's failure \
                 sound until the download finishes.";
    if count == 1 {
        return format!("Delete {title} ({mb} MB)? If SLC uses it, it downloads again {after}");
    }
    // The server only sends engines this version knows, but an unknown one
    // still reads sensibly by its own name.
    let engine = match engine_of(model) {
        Some(engine) => engine_name(engine),
        None => model.engine.as_str(),
    };
    format!(
        "Delete the {title} ({mb} MB)? This removes all {count} {engine} voices. \
         If SLC uses one, it downloads again {after}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::slc::dto::VoiceDto;
    use crate::catalog::{self, Archive};

    const KOKORO: &str = "kokoro-multi-lang-v1_0";
    const SUPERTONIC: &str = "sherpa-onnx-supertonic-3-tts-int8-2026-05-11";
    const JOE: &str = "vits-piper-en_US-joe-medium";

    /// Every archive in the catalog, once each, in catalog order.
    fn archives() -> Vec<&'static Archive> {
        let mut archives: Vec<&'static Archive> = Vec::new();
        for voice in catalog::voices() {
            if !archives.iter().any(|a| a.stem == voice.archive.stem) {
                archives.push(voice.archive);
            }
        }
        archives
    }

    /// `archive`'s model in `state`, with every catalog voice it holds.
    fn model(archive: &'static Archive, state: ModelState) -> ModelDto {
        let held: Vec<_> = catalog::voices()
            .iter()
            .filter(|voice| voice.archive.stem == archive.stem)
            .collect();
        ModelDto {
            stem: archive.stem.to_string(),
            engine: held[0].engine.as_str().to_string(),
            size: archive.size,
            state,
            voices: held
                .iter()
                .map(|voice| VoiceDto {
                    id: voice.id.to_string(),
                    name: voice.name.to_string(),
                    gender: None,
                    accent: None,
                    slow: voice.slow,
                    license: voice.license.to_string(),
                })
                .collect(),
        }
    }

    /// The whole catalog: Supertonic installed, everything else not.
    fn catalog_models() -> Vec<ModelDto> {
        archives()
            .into_iter()
            .map(|archive| {
                let state = if archive.stem == SUPERTONIC {
                    ModelState::Installed
                } else {
                    ModelState::NotInstalled
                };
                model(archive, state)
            })
            .collect()
    }

    fn archive(stem: &str) -> &'static Archive {
        archives().into_iter().find(|a| a.stem == stem).unwrap()
    }

    /// The one row `models` gives.
    fn only_row(models: &[ModelDto]) -> Row {
        let sections = sections(models, "", Filter::All);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].rows.len(), 1);
        sections[0].rows[0].clone()
    }

    fn row_in(state: ModelState, stem: &str) -> Row {
        only_row(&[model(archive(stem), state)])
    }

    fn failed(kind: FailureKind, reason: &str) -> ModelState {
        ModelState::Failed {
            reason: reason.to_string(),
            kind,
        }
    }

    #[test]
    fn rows_grouped_by_engine() {
        let models = catalog_models();
        let sections = sections(&models, "", Filter::All);
        let engines: Vec<Engine> = sections.iter().map(|s| s.engine).collect();
        assert_eq!(
            engines,
            vec![Engine::Supertonic, Engine::Kokoro, Engine::Piper]
        );

        let supertonic = &sections[0];
        assert_eq!(supertonic.title, "Supertonic");
        assert_eq!(
            supertonic.description,
            "Ten natural voices in one download. Quick on most PCs."
        );
        assert_eq!(supertonic.rows.len(), 1);
        assert_eq!(supertonic.rows[0].title, "Supertonic voices");
        assert_eq!(supertonic.rows[0].subline, "10 voices · 129 MB");
        assert_eq!(supertonic.rows[0].voices.len(), 10);
        assert!(
            supertonic.rows[0]
                .voices
                .contains(&("M3".to_string(), "M3 (Supertonic)".to_string()))
        );

        let kokoro = &sections[1];
        assert_eq!(
            kokoro.description,
            "28 expressive voices in one download. Slow on long announcements."
        );
        assert_eq!(kokoro.rows.len(), 1);
        assert_eq!(kokoro.rows[0].title, "Kokoro voices");
        assert_eq!(kokoro.rows[0].subline, "28 voices · 350 MB · slow");

        let piper = &sections[2];
        assert_eq!(
            piper.description,
            "One download per voice. The fastest voices, good for long announcements."
        );
        let piper_archives = archives()
            .into_iter()
            .filter(|a| a.stem.starts_with("vits-piper-"))
            .count();
        assert_eq!(piper.rows.len(), piper_archives);
        let joe = piper.rows.iter().find(|r| r.stem == JOE).unwrap();
        assert_eq!(joe.title, "Joe (US, medium, Piper)");
        assert_eq!(joe.subline, format!("{} MB", format_mb(archive(JOE).size)));
    }

    #[test]
    fn row_status_texts() {
        let installed = row_in(ModelState::Installed, JOE);
        assert_eq!(installed.status_text, "Installed");
        assert!(!installed.problem);
        assert_eq!(installed.progress, None);

        let not = row_in(ModelState::NotInstalled, JOE);
        assert_eq!(not.status_text, "Not installed");
        assert!(!not.problem);

        let total = archive(KOKORO).size;
        let downloading = row_in(
            ModelState::Downloading {
                done: 161_000_000,
                total,
            },
            KOKORO,
        );
        assert_eq!(downloading.status_text, "161 of 350 MB");
        let progress = downloading.progress.unwrap();
        assert!((progress - 161_000_000.0 / total as f32).abs() < 1e-4);
        assert!(!downloading.problem);

        let unpacking = row_in(ModelState::Downloading { done: total, total }, KOKORO);
        assert_eq!(unpacking.status_text, "Unpacking…");
        assert_eq!(unpacking.progress, Some(1.0));

        let network = row_in(failed(FailureKind::Network, "no internet connection"), JOE);
        assert_eq!(
            network.status_text,
            "Couldn't download: no internet connection. Check your connection, then press Download again."
        );
        assert!(network.problem);

        let permanent = row_in(
            failed(FailureKind::Permanent, "the file on the server has changed"),
            JOE,
        );
        assert_eq!(
            permanent.status_text,
            "Couldn't download: the file on the server has changed."
        );
        assert!(permanent.problem);

        let other = row_in(failed(FailureKind::Other, "the disk is full"), JOE);
        assert_eq!(
            other.status_text,
            "Couldn't download: the disk is full. Press Download to try again."
        );
        assert!(other.problem);
    }

    #[test]
    fn buttons_per_state() {
        // (can_play, can_download, can_delete)
        let buttons = |state: ModelState| {
            let row = row_in(state, JOE);
            (row.can_play, row.can_download, row.can_delete)
        };
        assert_eq!(buttons(ModelState::Installed), (true, false, true));
        assert_eq!(buttons(ModelState::NotInstalled), (false, true, false));
        assert_eq!(
            buttons(ModelState::Downloading { done: 1, total: 2 }),
            (false, false, false)
        );
        assert_eq!(
            buttons(ModelState::Downloading { done: 2, total: 2 }),
            (false, false, false)
        );
        assert_eq!(
            buttons(failed(FailureKind::Network, "no internet connection")),
            (false, true, false)
        );
        assert_eq!(
            buttons(failed(FailureKind::Other, "the disk is full")),
            (false, true, false)
        );
        assert_eq!(
            buttons(failed(FailureKind::Permanent, "the file changed")),
            (false, false, false)
        );
    }

    #[test]
    fn search_and_filter() {
        let models = catalog_models();
        let titles = |search: &str, filter: Filter| -> Vec<String> {
            sections(&models, search, filter)
                .into_iter()
                .flat_map(|s| s.rows)
                .map(|r| r.title)
                .collect()
        };

        assert_eq!(titles("joe", Filter::All), vec!["Joe (US, medium, Piper)"]);
        assert_eq!(titles("JOE", Filter::All), vec!["Joe (US, medium, Piper)"]);
        // A voice's ID finds the row that holds it.
        assert_eq!(titles("bm_george", Filter::All), vec!["Kokoro voices"]);
        // So does a voice's name.
        assert_eq!(titles("m3 (super", Filter::All), vec!["Supertonic voices"]);
        assert!(titles("no such voice", Filter::All).is_empty());

        // Only Supertonic is installed, so the other two sections go.
        let installed = sections(&models, "", Filter::Installed);
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].engine, Engine::Supertonic);

        let not_installed = sections(&models, "", Filter::NotInstalled);
        let engines: Vec<Engine> = not_installed.iter().map(|s| s.engine).collect();
        assert_eq!(engines, vec![Engine::Kokoro, Engine::Piper]);

        // Downloading and failed rows count as not installed.
        let mut busy = vec![
            model(
                archive(KOKORO),
                ModelState::Downloading { done: 1, total: 2 },
            ),
            model(
                archive(JOE),
                failed(FailureKind::Network, "no internet connection"),
            ),
        ];
        assert_eq!(sections(&busy, "", Filter::NotInstalled).len(), 2);
        assert!(sections(&busy, "", Filter::Installed).is_empty());

        // An empty search keeps everything; the search and filter combine.
        busy.push(model(archive(SUPERTONIC), ModelState::Installed));
        assert_eq!(sections(&busy, "  ", Filter::All).len(), 3);
        assert!(sections(&busy, "joe", Filter::Installed).is_empty());
    }

    #[test]
    fn play_voice_falls_back_to_first() {
        let supertonic = row_in(ModelState::Installed, SUPERTONIC);
        let first = supertonic.voices[0].0.clone();
        assert_eq!(play_voice(&supertonic, Some("M3")), Some("M3".to_string()));
        assert_eq!(play_voice(&supertonic, None), Some(first.clone()));
        // A pick the server no longer lists.
        assert_eq!(play_voice(&supertonic, Some("gone")), Some(first));

        let joe = row_in(ModelState::Installed, JOE);
        assert_eq!(play_voice(&joe, None), Some("en_US-joe-medium".to_string()));
        let mut empty = joe.clone();
        empty.voices.clear();
        assert_eq!(play_voice(&empty, None), None);
    }

    #[test]
    fn search_picks_matching_voice() {
        let kokoro = row_in(ModelState::Installed, KOKORO);
        assert_eq!(
            search_pick(&kokoro, "BM_George"),
            Some("bm_george".to_string())
        );
        // By name too, and the first match wins.
        let by_name = search_pick(&kokoro, "george").unwrap();
        assert_eq!(by_name, "bm_george");
        let supertonic = row_in(ModelState::Installed, SUPERTONIC);
        assert_eq!(
            search_pick(&supertonic, " m3 (super "),
            Some("M3".to_string())
        );
        // The row's own title names no one voice.
        assert_eq!(search_pick(&kokoro, "kokoro voices"), None);
        assert_eq!(search_pick(&kokoro, ""), None);
        assert_eq!(search_pick(&kokoro, "nobody"), None);
        // A one-voice row has no picker.
        let joe = row_in(ModelState::Installed, JOE);
        assert_eq!(search_pick(&joe, "joe"), None);
    }

    #[test]
    fn delete_target_only_while_installed() {
        let installed = vec![model(archive(JOE), ModelState::Installed)];
        assert_eq!(
            delete_target(&installed, JOE).map(|m| m.stem.as_str()),
            Some(JOE)
        );
        assert!(delete_target(&installed, KOKORO).is_none());
        for state in [
            ModelState::NotInstalled,
            ModelState::Downloading { done: 1, total: 2 },
            failed(FailureKind::Other, "the disk is full"),
        ] {
            let models = vec![model(archive(JOE), state)];
            assert!(delete_target(&models, JOE).is_none());
        }
    }

    #[test]
    fn delete_prompt_texts() {
        let kokoro = model(archive(KOKORO), ModelState::Installed);
        assert_eq!(
            delete_prompt(&kokoro),
            "Delete the Kokoro voices (350 MB)? This removes all 28 Kokoro voices. If SLC uses \
             one, it downloads again the next time SLC asks for it, and those announcements get \
             SLC's failure sound until the download finishes."
        );

        let supertonic = model(archive(SUPERTONIC), ModelState::Installed);
        assert!(
            delete_prompt(&supertonic).starts_with(
                "Delete the Supertonic voices (129 MB)? This removes all 10 Supertonic voices."
            ),
            "{}",
            delete_prompt(&supertonic)
        );

        let joe = model(archive(JOE), ModelState::Installed);
        assert_eq!(
            delete_prompt(&joe),
            format!(
                "Delete Joe (US, medium, Piper) ({} MB)? If SLC uses it, it downloads again the \
                 next time SLC asks for it, and those announcements get SLC's failure sound \
                 until the download finishes.",
                format_mb(archive(JOE).size)
            )
        );
    }
}
