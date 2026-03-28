use anyhow::Result;
use fabro_util::terminal::Styles;

use crate::args::RunsCommands;

pub(crate) mod inspect;
pub(crate) mod list;
pub(crate) mod rm;

pub async fn dispatch(cmd: RunsCommands) -> Result<()> {
    match cmd {
        RunsCommands::Ps(args) => {
            let styles = Styles::detect_stdout();
            list::list_command(&args, &styles)
        }
        RunsCommands::Rm(args) => rm::remove_command(&args).await,
        RunsCommands::Inspect(args) => inspect::run(&args),
    }
}

pub(super) fn short_run_id(id: &str) -> &str {
    if id.len() > 12 {
        &id[..12]
    } else {
        id
    }
}

#[cfg(test)]
mod tests {
    use crate::commands::system::parse_duration;
    use crate::shared::format_size;

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration("24h").unwrap(), chrono::Duration::hours(24));
    }

    #[test]
    fn parse_duration_days() {
        assert_eq!(parse_duration("7d").unwrap(), chrono::Duration::days(7));
    }

    #[test]
    fn parse_duration_rejects_invalid_unit() {
        let err = parse_duration("5m").unwrap_err();
        assert!(err.to_string().contains("invalid duration unit"));
    }

    #[test]
    fn format_size_humanizes_thresholds() {
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
    }
}
