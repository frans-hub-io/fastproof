//! FPQL: the query language of cases and requirements (§8). Parsing, the AST and the compiler
//! to SQL fragments live in the submodules.

pub mod ast;
pub mod compile;
pub mod lexer;
pub mod parser;
pub mod requirements;
