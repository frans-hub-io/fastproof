//! FPQL lexer (§8): words, quoted strings, operators and punctuation with byte spans. A word
//! is `[letter|digit|_][letter|digit|_.:/-]*` (letters include non-ASCII ones), so `TC-12`,
//! `REQ-3`, `2026-01-31` and `7d` are single words; the parser gives them a meaning.

use super::ast::Span;

/// A token kind.
#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Word(String),
    /// A quoted string (escapes resolved).
    Str(String),
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Eq,
    Ne,
    Tilde,
    NotTilde,
    Gt,
    Ge,
    Lt,
    Le,
    Minus,
    Plus,
    Eof,
}

impl Tok {
    /// How the token reads in an error message.
    pub fn describe(&self) -> String {
        match self {
            Tok::Word(w) => format!("{w:?}"),
            Tok::Str(_) => "a string".into(),
            Tok::LParen => "\"(\"".into(),
            Tok::RParen => "\")\"".into(),
            Tok::LBracket => "\"[\"".into(),
            Tok::RBracket => "\"]\"".into(),
            Tok::Comma => "\",\"".into(),
            Tok::Eq => "\"=\"".into(),
            Tok::Ne => "\"!=\"".into(),
            Tok::Tilde => "\"~\"".into(),
            Tok::NotTilde => "\"!~\"".into(),
            Tok::Gt => "\">\"".into(),
            Tok::Ge => "\">=\"".into(),
            Tok::Lt => "\"<\"".into(),
            Tok::Le => "\"<=\"".into(),
            Tok::Minus => "\"-\"".into(),
            Tok::Plus => "\"+\"".into(),
            Tok::Eof => "the end of the query".into(),
        }
    }
}

/// A token with its byte span.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

/// A lexing error: what went wrong where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexError {
    pub span: Span,
    pub message: String,
    pub expected: Vec<String>,
}

fn word_start(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn word_char(c: char) -> bool {
    word_start(c) || matches!(c, '.' | ':' | '/' | '-')
}

/// Whether `s` lexes as one bare word.
pub fn is_word(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(word_start) && cs.all(word_char)
}

/// Splits a query into tokens (the last one is `Eof`).
pub fn lex(src: &str) -> Result<Vec<Token>, LexError> {
    let mut out = Vec::new();
    let mut it = src.char_indices().peekable();
    while let Some(&(i, c)) = it.peek() {
        if matches!(c, ' ' | '\t' | '\n' | '\r') {
            it.next();
            continue;
        }
        if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}' | '\u{feff}') {
            return Err(LexError {
                span: Span::new(i, i + c.len_utf8()),
                message: "control characters are not allowed".into(),
                expected: Vec::new(),
            });
        }
        let one = |t: Tok| Token {
            tok: t,
            span: Span::new(i, i + 1),
        };
        match c {
            '(' | ')' | '[' | ']' | ',' | '=' | '~' | '-' | '+' => {
                it.next();
                out.push(one(match c {
                    '(' => Tok::LParen,
                    ')' => Tok::RParen,
                    '[' => Tok::LBracket,
                    ']' => Tok::RBracket,
                    ',' => Tok::Comma,
                    '=' => Tok::Eq,
                    '~' => Tok::Tilde,
                    '-' => Tok::Minus,
                    _ => Tok::Plus,
                }));
            }
            '!' | '>' | '<' => {
                it.next();
                let eq = it.peek().map(|&(_, n)| n);
                let (tok, two) = match (c, eq) {
                    ('!', Some('=')) => (Tok::Ne, true),
                    ('!', Some('~')) => (Tok::NotTilde, true),
                    ('>', Some('=')) => (Tok::Ge, true),
                    ('<', Some('=')) => (Tok::Le, true),
                    ('>', _) => (Tok::Gt, false),
                    ('<', _) => (Tok::Lt, false),
                    _ => {
                        return Err(LexError {
                            span: Span::new(i, i + 1),
                            message: "\"!\" must be followed by \"=\" or \"~\"".into(),
                            expected: vec!["!=".into(), "!~".into()],
                        });
                    }
                };
                if two {
                    it.next();
                }
                out.push(Token {
                    tok,
                    span: Span::new(i, i + if two { 2 } else { 1 }),
                });
            }
            '"' | '\'' => {
                it.next();
                let mut s = String::new();
                let mut end = None;
                while let Some((j, d)) = it.next() {
                    if d == c {
                        end = Some(j + 1);
                        break;
                    }
                    if d.is_control() && d != '\t' {
                        return Err(LexError {
                            span: Span::new(j, j + d.len_utf8()),
                            message: "control characters are not allowed".into(),
                            expected: Vec::new(),
                        });
                    }
                    if d == '\\' {
                        match it.next() {
                            Some((k, e)) if e.is_control() => {
                                return Err(LexError {
                                    span: Span::new(k, k + e.len_utf8()),
                                    message: "control characters are not allowed".into(),
                                    expected: Vec::new(),
                                });
                            }
                            Some((_, e)) => s.push(e),
                            None => break,
                        }
                    } else {
                        s.push(d);
                    }
                }
                let Some(end) = end else {
                    return Err(LexError {
                        span: Span::new(i, src.len()),
                        message: "the string is not closed".into(),
                        expected: vec![c.to_string()],
                    });
                };
                out.push(Token {
                    tok: Tok::Str(s),
                    span: Span::new(i, end),
                });
            }
            c if word_start(c) => {
                let mut end = i;
                while let Some(&(j, d)) = it.peek() {
                    if !word_char(d) {
                        break;
                    }
                    end = j + d.len_utf8();
                    it.next();
                }
                out.push(Token {
                    tok: Tok::Word(src[i..end].to_string()),
                    span: Span::new(i, end),
                });
            }
            _ => {
                return Err(LexError {
                    span: Span::new(i, i + c.len_utf8()),
                    message: format!("unexpected character {c:?}"),
                    expected: Vec::new(),
                });
            }
        }
    }
    out.push(Token {
        tok: Tok::Eof,
        span: Span::new(src.len(), src.len()),
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Tok> {
        lex(s).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn words_strings_and_operators() {
        assert_eq!(
            toks("number>=TC-12 and name!~'a\\'b'"),
            vec![
                Tok::Word("number".into()),
                Tok::Ge,
                Tok::Word("TC-12".into()),
                Tok::Word("and".into()),
                Tok::Word("name".into()),
                Tok::NotTilde,
                Tok::Str("a'b".into()),
                Tok::Eof
            ]
        );
        assert_eq!(
            toks("lastRunAt<now()-7d"),
            vec![
                Tok::Word("lastRunAt".into()),
                Tok::Lt,
                Tok::Word("now".into()),
                Tok::LParen,
                Tok::RParen,
                Tok::Minus,
                Tok::Word("7d".into()),
                Tok::Eof
            ]
        );
        assert_eq!(
            toks("tag = тест"),
            vec![
                Tok::Word("tag".into()),
                Tok::Eq,
                Tok::Word("тест".into()),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn errors_point_at_the_problem() {
        let e = lex("name = \"abc").unwrap_err();
        assert_eq!(e.span, Span::new(7, 11));
        let e = lex("a ! b").unwrap_err();
        assert_eq!(e.span, Span::new(2, 3));
        let e = lex("a = \u{1}").unwrap_err();
        assert_eq!(e.span.start, 4);
        let e = lex("a = $1").unwrap_err();
        assert_eq!(e.span, Span::new(4, 5));
        assert!(lex("a = \"x\\").is_err());
    }
}
