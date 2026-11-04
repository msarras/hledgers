//! hledger's functional test corpus (shelltestrunner `.test` files) run against
//! the hledgers binary, without shelltestrunner or a system shell.

pub mod format;
mod run;

pub use run::main;
