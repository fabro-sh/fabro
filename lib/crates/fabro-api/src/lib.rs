#[allow(
    clippy::absolute_paths,
    clippy::all,
    clippy::derivable_impls,
    clippy::needless_lifetimes,
    unreachable_pub,
    unused_imports
)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/codegen.rs"));
}
pub use generated::Client;
pub use generated::types;
