/// Condition expression parser for edge guards (spec Section 10).
///
/// Grammar:
/// ```text
/// Expr        ::= OrExpr
/// OrExpr      ::= AndExpr ('||' AndExpr)*
/// AndExpr     ::= UnaryExpr ('&&' UnaryExpr)*
/// UnaryExpr   ::= '!' UnaryExpr | Clause
/// Clause      ::= Key Op Literal | Key        (bare key = truthy)
/// Op          ::= '=' | '!=' | '>' | '<' | '>=' | '<='
///               | 'contains' | 'matches'
/// Literal     ::= String | Integer | Boolean | BareLiteral
/// BareLiteral ::= [A-Za-z_][A-Za-z0-9_.:-]*
/// ```
use crate::error::GraphvizError;

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ConditionExpr {
    Clause(Clause),
    Not(Box<Self>),
    And(Vec<Self>),
    Or(Vec<Self>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Clause {
    pub key:   String,
    pub op:    Op,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Eq,
    NotEq,
    Gt,
    Lt,
    Gte,
    Lte,
    Contains,
    Matches,
    Truthy,
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    OpEq,     // =
    OpNotEq,  // !=
    OpGt,     // >
    OpLt,     // <
    OpGte,    // >=
    OpLte,    // <=
    And,      // &&
    Or,       // ||
    Not,      // !
    Contains, // contains
    Matches,  // matches
}

fn tokenize(input: &str) -> Result<Vec<Token>, GraphvizError> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(Vec::new());
    }

    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut tokens = Vec::new();

    while i < len {
        // Skip whitespace
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }

        // Two-char operators (longest match first)
        if i + 1 < len {
            let two = format!("{}{}", chars[i], chars[i + 1]);
            match two.as_str() {
                "&&" => {
                    tokens.push(Token::And);
                    i += 2;
                    continue;
                }
                "||" => {
                    tokens.push(Token::Or);
                    i += 2;
                    continue;
                }
                "!=" => {
                    tokens.push(Token::OpNotEq);
                    i += 2;
                    continue;
                }
                ">=" => {
                    tokens.push(Token::OpGte);
                    i += 2;
                    continue;
                }
                "<=" => {
                    tokens.push(Token::OpLte);
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }

        // Single-char operators
        match chars[i] {
            '=' => {
                tokens.push(Token::OpEq);
                i += 1;
                continue;
            }
            '>' => {
                tokens.push(Token::OpGt);
                i += 1;
                continue;
            }
            '<' => {
                tokens.push(Token::OpLt);
                i += 1;
                continue;
            }
            '!' => {
                tokens.push(Token::Not);
                i += 1;
                continue;
            }
            _ => {}
        }

        // Quoted string: consume `"..."` as a single word token
        if chars[i] == '"' {
            let start = i;
            i += 1; // skip opening quote
            while i < len && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < len {
                    i += 1; // skip escaped char
                }
                i += 1;
            }
            if i < len {
                i += 1; // skip closing quote
            }
            let word: String = chars[start..i].iter().collect();
            tokens.push(Token::Word(word));
            continue;
        }

        // Word: everything up to whitespace or operator char
        let start = i;
        while i < len && !chars[i].is_whitespace() && !is_op_char(chars[i]) && chars[i] != '"' {
            i += 1;
        }
        if i == start {
            return Err(GraphvizError::Parse(format!(
                "unexpected character '{}' in condition expression",
                chars[i]
            )));
        }
        let word: String = chars[start..i].iter().collect();

        // Recognize keyword operators only when they appear between words
        // (not as the first or last token, and not adjacent to another operator)
        match word.as_str() {
            "contains" if is_word_operator_context(&tokens) => {
                tokens.push(Token::Contains);
            }
            "matches" if is_word_operator_context(&tokens) => {
                tokens.push(Token::Matches);
            }
            _ => {
                tokens.push(Token::Word(word));
            }
        }
    }

    Ok(tokens)
}

fn is_op_char(c: char) -> bool {
    matches!(c, '=' | '!' | '>' | '<' | '&' | '|')
}

/// Word operators (`contains`, `matches`) are recognized when preceded by a
/// Word token.
fn is_word_operator_context(tokens: &[Token]) -> bool {
    matches!(tokens.last(), Some(Token::Word(_)))
}

// ---------------------------------------------------------------------------
// Parser (recursive descent)
// ---------------------------------------------------------------------------

struct Parser {
    tokens: Vec<Token>,
    pos:    usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<Token> {
        let tok = self.tokens.get(self.pos).cloned();
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    fn parse_expr(&mut self) -> Result<ConditionExpr, GraphvizError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<ConditionExpr, GraphvizError> {
        let mut children = vec![self.parse_and()?];
        while self.peek() == Some(&Token::Or) {
            self.advance();
            children.push(self.parse_and()?);
        }
        if children.len() == 1 {
            Ok(children.pop().expect("just checked length"))
        } else {
            Ok(ConditionExpr::Or(children))
        }
    }

    fn parse_and(&mut self) -> Result<ConditionExpr, GraphvizError> {
        let mut children = vec![self.parse_unary()?];
        while self.peek() == Some(&Token::And) {
            self.advance();
            children.push(self.parse_unary()?);
        }
        if children.len() == 1 {
            Ok(children.pop().expect("just checked length"))
        } else {
            Ok(ConditionExpr::And(children))
        }
    }

    fn parse_unary(&mut self) -> Result<ConditionExpr, GraphvizError> {
        if self.peek() == Some(&Token::Not) {
            self.advance();
            let inner = self.parse_unary()?;
            return Ok(ConditionExpr::Not(Box::new(inner)));
        }
        self.parse_clause()
    }

    fn parse_clause(&mut self) -> Result<ConditionExpr, GraphvizError> {
        let key = match self.advance() {
            Some(Token::Word(w)) => w,
            Some(other) => {
                return Err(GraphvizError::Parse(format!(
                    "expected key, got {other:?} in condition expression"
                )));
            }
            None => {
                return Err(GraphvizError::Parse(
                    "unexpected end of condition expression".to_string(),
                ));
            }
        };

        // Check for operator
        let op = match self.peek() {
            Some(Token::OpEq) => Some(Op::Eq),
            Some(Token::OpNotEq) => Some(Op::NotEq),
            Some(Token::OpGt) => Some(Op::Gt),
            Some(Token::OpLt) => Some(Op::Lt),
            Some(Token::OpGte) => Some(Op::Gte),
            Some(Token::OpLte) => Some(Op::Lte),
            Some(Token::Contains) => Some(Op::Contains),
            Some(Token::Matches) => Some(Op::Matches),
            _ => None,
        };

        let Some(op) = op else {
            // Bare key -> truthy
            return Ok(ConditionExpr::Clause(Clause {
                key,
                op: Op::Truthy,
                value: String::new(),
            }));
        };

        self.advance(); // consume the operator

        // Value: must be a Word
        let value = match self.advance() {
            Some(Token::Word(w)) => parse_literal(&w),
            Some(other) => {
                return Err(GraphvizError::Parse(format!(
                    "expected value after operator, got {other:?}"
                )));
            }
            None => {
                // Allow empty value for `=` and `!=` (backward compat: `missing_key=`)
                if op == Op::Eq || op == Op::NotEq {
                    String::new()
                } else {
                    return Err(GraphvizError::Parse(
                        "expected value after operator".to_string(),
                    ));
                }
            }
        };

        // Validate regex at parse time
        if op == Op::Matches {
            regex::Regex::new(&value).map_err(|e| {
                GraphvizError::Parse(format!("invalid regex pattern '{value}': {e}"))
            })?;
        }

        Ok(ConditionExpr::Clause(Clause { key, op, value }))
    }
}

/// Strip surrounding double-quotes from a literal value.
/// Bare values pass through unchanged.
fn parse_literal(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        trimmed.to_string()
    }
}

fn parse_expression(expr: &str) -> Result<ConditionExpr, GraphvizError> {
    let tokens = tokenize(expr)?;
    if tokens.is_empty() {
        return Ok(ConditionExpr::And(Vec::new()));
    }
    let mut parser = Parser::new(tokens);
    let result = parser.parse_expr()?;
    if parser.pos < parser.tokens.len() {
        return Err(GraphvizError::Parse(format!(
            "unexpected token {:?} in condition expression",
            parser.tokens[parser.pos]
        )));
    }
    Ok(result)
}

/// Parse and validate a condition expression.
///
/// # Errors
///
/// Returns an error if the expression contains invalid syntax.
pub fn parse_condition(expr: &str) -> Result<(), GraphvizError> {
    parse_expression(expr)?;
    Ok(())
}

/// Parse a condition expression and return the AST.
///
/// # Errors
///
/// Returns an error if the expression contains invalid syntax.
pub fn parse_condition_expr(expr: &str) -> Result<ConditionExpr, GraphvizError> {
    parse_expression(expr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_condition_validates() {
        assert!(parse_condition("outcome=success").is_ok());
        assert!(parse_condition("outcome=success && context.x=y").is_ok());
        assert!(parse_condition("").is_ok());
    }

    #[test]
    fn parse_condition_accepts_bare_key() {
        assert!(parse_condition("some_flag").is_ok());
    }

    #[test]
    fn parse_eq_into_clause() {
        let expr = parse_expression("outcome=success").unwrap();
        assert_eq!(
            expr,
            ConditionExpr::Clause(Clause {
                key:   "outcome".to_string(),
                op:    Op::Eq,
                value: "success".to_string(),
            })
        );
    }

    #[test]
    fn parse_and_into_and_node() {
        let expr = parse_expression("a=1 && b=2").unwrap();
        assert_eq!(
            expr,
            ConditionExpr::And(vec![
                ConditionExpr::Clause(Clause {
                    key:   "a".to_string(),
                    op:    Op::Eq,
                    value: "1".to_string(),
                }),
                ConditionExpr::Clause(Clause {
                    key:   "b".to_string(),
                    op:    Op::Eq,
                    value: "2".to_string(),
                }),
            ])
        );
    }

    #[test]
    fn parse_bare_key_into_truthy() {
        let expr = parse_expression("some_flag").unwrap();
        assert_eq!(
            expr,
            ConditionExpr::Clause(Clause {
                key:   "some_flag".to_string(),
                op:    Op::Truthy,
                value: String::new(),
            })
        );
    }

    #[test]
    fn parse_not_eq_into_clause() {
        let expr = parse_expression("outcome!=fail").unwrap();
        assert_eq!(
            expr,
            ConditionExpr::Clause(Clause {
                key:   "outcome".to_string(),
                op:    Op::NotEq,
                value: "fail".to_string(),
            })
        );
    }

    #[test]
    fn parse_numeric_comparisons() {
        assert!(parse_condition("x > 5").is_ok());
        assert!(parse_condition("x >= 5").is_ok());
        assert!(parse_condition("x < 5").is_ok());
        assert!(parse_condition("x <= 5").is_ok());
    }

    #[test]
    fn parse_contains() {
        assert!(parse_condition("x contains y").is_ok());
    }

    #[test]
    fn parse_matches() {
        assert!(parse_condition("x matches ^ok$").is_ok());
    }

    #[test]
    fn matches_invalid_regex_fails_parse() {
        assert!(parse_condition("x matches [bad").is_err());
    }

    #[test]
    fn parse_or() {
        assert!(parse_condition("a=1 || b=2").is_ok());
    }

    #[test]
    fn parse_not() {
        assert!(parse_condition("!x=y").is_ok());
    }

    // -----------------------------------------------------------------------
    // parse_literal: quote stripping
    // -----------------------------------------------------------------------

    #[test]
    fn parse_literal_bare_value() {
        assert_eq!(parse_literal("success"), "success");
    }

    #[test]
    fn parse_literal_strips_quotes() {
        assert_eq!(parse_literal("\"success\""), "success");
    }

    #[test]
    fn parse_literal_unescapes_inner_quotes() {
        assert_eq!(parse_literal(r#""say \"hello\"""#), r#"say "hello""#);
    }

    #[test]
    fn parse_literal_single_quote_not_stripped() {
        // Only double-quotes are stripped
        assert_eq!(parse_literal("\"partial"), "\"partial");
    }

    #[test]
    fn parse_literal_empty_quoted_string() {
        assert_eq!(parse_literal("\"\""), "");
    }

    // -----------------------------------------------------------------------
    // Quoted string values in conditions
    // -----------------------------------------------------------------------

    #[test]
    fn quoted_value_stripped_in_clause() {
        let expr = parse_expression(r#"outcome="success""#).unwrap();
        assert_eq!(
            expr,
            ConditionExpr::Clause(Clause {
                key:   "outcome".to_string(),
                op:    Op::Eq,
                value: "success".to_string(),
            })
        );
    }

    #[test]
    fn bare_and_quoted_values_produce_same_clause() {
        let bare = parse_expression("outcome=success").unwrap();
        let quoted = parse_expression(r#"outcome="success""#).unwrap();
        assert_eq!(bare, quoted);
    }

    #[test]
    fn quoted_value_with_not_eq() {
        let expr = parse_expression(r#"outcome!="fail""#).unwrap();
        assert_eq!(
            expr,
            ConditionExpr::Clause(Clause {
                key:   "outcome".to_string(),
                op:    Op::NotEq,
                value: "fail".to_string(),
            })
        );
    }

    #[test]
    fn quoted_value_with_spaces() {
        let expr = parse_expression(r#"context.msg="hello world""#).unwrap();
        assert_eq!(
            expr,
            ConditionExpr::Clause(Clause {
                key:   "context.msg".to_string(),
                op:    Op::Eq,
                value: "hello world".to_string(),
            })
        );
    }
}
