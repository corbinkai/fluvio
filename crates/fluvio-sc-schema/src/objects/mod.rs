mod create;
mod delete;
mod update;
mod list;
mod watch;
mod metadata;
mod clear;

// backward compatibility with classic protocol. this should go away once we deprecate classic
pub mod classic;

pub use create::*;
pub use update::*;
pub use delete::*;
pub use list::*;
pub use watch::*;
pub use metadata::*;
pub use clear::*;

pub(crate) const COMMON_VERSION: i16 = 19; // from now, we use a single version for all objects
pub(crate) const DYN_OBJ: i16 = 11; // version indicate dynamic object

#[cfg(test)]
mod test;
