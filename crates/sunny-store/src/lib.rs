//! sunny-store: Session and symbol persistence for Sunny chat
//!
//! Provides SQLite-backed storage for chat sessions, messages, and code symbols.

pub mod context_file;
pub mod db;
pub mod error;
pub mod index;
pub mod session;
pub mod token_budget;

pub use db::Database;
pub use error::StoreError;
pub use index::{IndexStats, Symbol, SymbolIndex, SymbolKind};
pub use session::{SavedSession, SessionStore};
pub use token_budget::TokenBudget;
