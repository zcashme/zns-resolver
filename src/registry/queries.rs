//! Read-only query implementations and helpers.
//!
//! These do not participate in write transactions. The main entry points
//! are exposed via the `Registry` handle (which routes through the DB thread).

// Read query helpers can live here in the future.
// For now the DbConn methods (exposed via the actor) contain the
// implementations so that transactional and non-transactional paths
// are easy to reason about from core.rs.

#[allow(dead_code)]
pub(crate) fn _queries_module_present() {}
