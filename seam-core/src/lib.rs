//! seam's core: what a change did to the named things in it, from the two
//! sides of each changed file and the diff between them. No git, no files.
//! The share/ files its comments name are the shell steps of seam
//! (https://github.com/booka66/seam), which this mirrors.
pub mod decls;
pub mod diffs;
pub mod graph;
pub mod hunks;
mod imports;
pub mod kinds;
pub mod table;

pub use decls::{Lang, Rules};
pub use diffs::diffs;
pub use graph::{graph, order, Ends};
pub use table::{table, Change};

