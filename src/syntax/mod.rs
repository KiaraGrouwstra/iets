pub mod ast;
pub mod symbol;

#[cfg(test)]
pub mod test_builder;

use ast::{Ast, FileId, Span};
use symbol::Interner;

#[derive(Debug)]
pub struct ParseError {
    pub span: Span,
    pub msg: String,
}

// STUB: replaced by branch parser
pub fn parse(_src: &[u8], _file: FileId, _interner: &mut Interner) -> Result<Ast, ParseError> {
    Err(ParseError {
        span: Span::default(),
        msg: "parser not linked yet".into(),
    })
}
