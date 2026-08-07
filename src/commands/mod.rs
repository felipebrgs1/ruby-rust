//! Comandos do CLI — um modulo por comando/dominio (estrutura inspirada no cli/ do Deno).




pub mod build;
pub mod deps;
pub mod doctor;
pub mod exec;
pub mod repl;
pub mod run;
pub mod serve;
pub mod task;
pub mod test;
pub mod tooling;

pub use build::cmd_build;
pub use deps::cmd_bundle_wrapper;
pub use doctor::{cmd_doctor, cmd_status, cmd_stop};
pub use exec::cmd_exec;
pub use repl::cmd_repl;
pub use run::cmd_run;
pub use serve::cmd_serve;
pub use task::cmd_task;
pub use test::cmd_test;
pub use tooling::{cmd_completions, cmd_init, cmd_upgrade};
