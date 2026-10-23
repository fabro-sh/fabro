//! Terminal styling for an already interpreted stage outcome. Decoding engine
//! events and deciding run success remain the callers' responsibilities.

use fabro_types::StageOutcome;
use fabro_util::terminal::Styles;

pub(super) struct StagePresentation {
    pub(super) glyph: String,
    pub(super) label: Option<String>,
}

impl StagePresentation {
    pub(super) fn new(outcome: StageOutcome, styles: &Styles) -> Self {
        let (glyph, label) = match outcome {
            StageOutcome::Succeeded => (styles.green.apply_to("\u{2713}").to_string(), None),
            StageOutcome::PartiallySucceeded => (
                styles.green.apply_to("\u{2713}").to_string(),
                Some(styles.yellow.apply_to("partial").to_string()),
            ),
            StageOutcome::Skipped => (
                styles.dim.apply_to("\u{2298}").to_string(),
                Some(styles.dim.apply_to("skipped").to_string()),
            ),
            StageOutcome::Failed { .. } => (styles.red.apply_to("\u{2717}").to_string(), None),
        };
        Self { glyph, label }
    }
}
