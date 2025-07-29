#![allow(clippy::expect_fun_call)]
#![feature(path_file_prefix)]
pub mod accounts_db;
pub mod compile;
pub mod error;
pub mod precompiles;
pub mod seashell;
pub mod spl;
pub mod sysvar;

pub use seashell::*;

pub fn set_log() {
    unsafe { std::env::set_var("RUST_LOG", "debug") }
}
