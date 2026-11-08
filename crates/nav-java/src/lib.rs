//! Java 8 parsing, symbol extraction and framework providers.
//!
//! Scope resolution in this P0 skeleton is name-based with a same-file preference.
//! A lexical scope stack and overload resolution are planned for P1.

pub mod lombok;
pub mod mybatis;
pub mod parser;

pub use mybatis::MybatisProvider;
pub use parser::JavaParser;
