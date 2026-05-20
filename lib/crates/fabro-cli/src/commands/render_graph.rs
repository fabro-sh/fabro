#![expect(
    clippy::disallowed_types,
    reason = "sync CLI `render-graph` command: reads DOT from stdin and writes rendered output \
              to stdout via blocking std::io"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "sync CLI `render-graph` command: uses blocking std::io::stdin/stdout"
)]

use std::io::{Read, Write};

use fabro_graphviz::render;

const RENDER_ERROR_PREFIX: &str = "RENDER_ERROR:";

pub(crate) fn execute() -> i32 {
    let mut dot_source = String::new();
    if std::io::stdin().read_to_string(&mut dot_source).is_err() {
        return 1;
    }

    let dot = render::RenderableDot::from_fabro_source(&dot_source);
    match render::render_raw_svg(&dot) {
        Ok(svg) => {
            if std::io::stdout().write_all(&svg).is_err() {
                return 1;
            }
            0
        }
        Err(err) => {
            if write!(std::io::stdout(), "{RENDER_ERROR_PREFIX}{err}").is_err() {
                return 1;
            }
            0
        }
    }
}
