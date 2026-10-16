/// Lightweight SQL tokenizer for syntax-highlighting history entries.
///
/// Only used for display — this is not a parser. It doesn't need to understand
/// SQL structure, only recognize token kinds so we can color them. Keyword /
/// function sets + the `#`-as-line-comment quirk come from the active
/// [`DbmsPlane`].
use crate::plane::DbmsPlane;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TokenKind {
    /// Clause-introducing keyword — SELECT / FROM / WHERE / JOIN / etc.
    /// Rendered bold so the query skeleton pops out from its decoration.
    KeywordMajor,
    Keyword,
    Function,
    StringLit,
    NumLit,
    LineComment,
    BlockComment,
    Backtick,
    Operator,
    /// Grouping brackets: ( ) [ ] { } — rendered bold so nested structure is
    /// easy to scan. Separated from `Punctuation` because brackets carry
    /// structural meaning while commas/semicolons/dots are mere separators.
    Bracket,
    /// Inline separators: , ; . — kept quiet so they don't compete for
    /// attention with keywords and brackets.
    Punctuation,
    Plain,
}

impl TokenKind {
    pub fn css_class(self) -> &'static str {
        match self {
            TokenKind::KeywordMajor => "sql-kw-major",
            TokenKind::Keyword      => "sql-kw",
            TokenKind::Function     => "sql-fn",
            TokenKind::StringLit    => "sql-str",
            TokenKind::NumLit       => "sql-num",
            TokenKind::LineComment
            | TokenKind::BlockComment => "sql-comment",
            TokenKind::Backtick     => "sql-bt",
            TokenKind::Operator     => "sql-op",
            TokenKind::Bracket      => "sql-bracket",
            TokenKind::Punctuation  => "sql-punct",
            TokenKind::Plain        => "sql-plain",
        }
    }
}

pub struct Token {
    pub text: String,
    pub kind: TokenKind,
}

pub fn tokenize_sql(sql: &str, plane: &'static dyn DbmsPlane) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = sql.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let major_keywords = plane.major_keywords();
    let set_operators = plane.set_operators();
    let keywords = plane.keywords();
    let functions = plane.functions();
    let hash_is_comment = plane.lexicon().hash_comments();

    while i < len {
        let c = chars[i];

        // ── Single-quoted string ──────────────────────────────────────────────
        if c == '\'' {
            let start = i;
            i += 1;
            loop {
                if i >= len { break; }
                if chars[i] == '\'' {
                    i += 1;
                    if i < len && chars[i] == '\'' { i += 1; } // '' escape
                    else { break; }
                } else if chars[i] == '\\' && i + 1 < len {
                    i += 2; // \' or \\
                } else {
                    i += 1;
                }
            }
            tokens.push(Token { text: chars[start..i].iter().collect(), kind: TokenKind::StringLit });
            continue;
        }

        // ── Double-quoted string / identifier ─────────────────────────────────
        if c == '"' {
            let start = i;
            i += 1;
            loop {
                if i >= len { break; }
                if chars[i] == '"' {
                    i += 1;
                    if i < len && chars[i] == '"' { i += 1; } // "" escape
                    else { break; }
                } else if chars[i] == '\\' && i + 1 < len {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            tokens.push(Token { text: chars[start..i].iter().collect(), kind: TokenKind::StringLit });
            continue;
        }

        // ── Backtick-quoted identifier ────────────────────────────────────────
        if c == '`' {
            let start = i;
            i += 1;
            while i < len && chars[i] != '`' { i += 1; }
            if i < len { i += 1; }
            tokens.push(Token { text: chars[start..i].iter().collect(), kind: TokenKind::Backtick });
            continue;
        }

        // ── Line comment: -- ─────────────────────────────────────────────────
        if c == '-' && i + 1 < len && chars[i + 1] == '-' {
            let start = i;
            while i < len && chars[i] != '\n' { i += 1; }
            tokens.push(Token { text: chars[start..i].iter().collect(), kind: TokenKind::LineComment });
            continue;
        }

        // ── Block comment: /* ... */ ──────────────────────────────────────────
        if c == '/' && i + 1 < len && chars[i + 1] == '*' {
            let start = i;
            i += 2;
            while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '/') { i += 1; }
            if i + 1 < len { i += 2; }
            tokens.push(Token { text: chars[start..i].iter().collect(), kind: TokenKind::BlockComment });
            continue;
        }

        // ── Hash comment: # ... (MySQL extension; Postgres doesn't accept it) ─
        if c == '#' && hash_is_comment {
            let start = i;
            while i < len && chars[i] != '\n' { i += 1; }
            tokens.push(Token { text: chars[start..i].iter().collect(), kind: TokenKind::LineComment });
            continue;
        }

        // ── Number literal ────────────────────────────────────────────────────
        if c.is_ascii_digit() || (c == '.' && i + 1 < len && chars[i + 1].is_ascii_digit()) {
            let start = i;
            if c == '0' && i + 1 < len && matches!(chars[i + 1], 'x' | 'X') {
                i += 2;
                while i < len && chars[i].is_ascii_hexdigit() { i += 1; }
            } else {
                while i < len && (chars[i].is_ascii_digit() || chars[i] == '.') { i += 1; }
                if i < len && matches!(chars[i], 'e' | 'E') {
                    i += 1;
                    if i < len && matches!(chars[i], '+' | '-') { i += 1; }
                    while i < len && chars[i].is_ascii_digit() { i += 1; }
                }
            }
            tokens.push(Token { text: chars[start..i].iter().collect(), kind: TokenKind::NumLit });
            continue;
        }

        // ── Word: keyword, function name, or plain identifier ─────────────────
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') { i += 1; }
            let word: String = chars[start..i].iter().collect();
            let upper = word.to_ascii_uppercase();
            let kind = if major_keywords.binary_search(&upper.as_str()).is_ok() {
                TokenKind::KeywordMajor
            } else if set_operators.binary_search(&upper.as_str()).is_ok() {
                TokenKind::Operator
            } else if keywords.binary_search(&upper.as_str()).is_ok() {
                TokenKind::Keyword
            } else if functions.binary_search(&upper.as_str()).is_ok() {
                TokenKind::Function
            } else {
                TokenKind::Plain
            };
            tokens.push(Token { text: word, kind });
            continue;
        }

        // ── Whitespace — group runs together ──────────────────────────────────
        if c.is_whitespace() {
            let start = i;
            while i < len && chars[i].is_whitespace() { i += 1; }
            tokens.push(Token { text: chars[start..i].iter().collect(), kind: TokenKind::Plain });
            continue;
        }

        // ── Grouping brackets: ( ) [ ] { } ────────────────────────────────
        if matches!(c, '(' | ')' | '[' | ']' | '{' | '}') {
            tokens.push(Token { text: c.to_string(), kind: TokenKind::Bracket });
            i += 1;
            continue;
        }

        // ── Inline separators: , ; . ──────────────────────────────────────
        // Placed after the number arm so `.5` and `1.5` stay numbers — only
        // a `.` between non-numeric neighbours reaches here.
        if matches!(c, ',' | ';' | '.') {
            tokens.push(Token { text: c.to_string(), kind: TokenKind::Punctuation });
            i += 1;
            continue;
        }

        // ── Operators — detect multi-char sequences ─────────────────────────
        let op_len = match c {
            '-' if i + 2 < len && chars[i+1] == '>' && chars[i+2] == '>' => 3, // ->>
            '-' if i + 1 < len && chars[i+1] == '>'                       => 2, // ->
            '|' if i + 1 < len && chars[i+1] == '|'                       => 2, // ||
            '<' if i + 1 < len && chars[i+1] == '>'                       => 2, // <>
            '<' if i + 1 < len && chars[i+1] == '='                       => 2, // <=
            '>' if i + 1 < len && chars[i+1] == '='                       => 2, // >=
            '!' if i + 1 < len && chars[i+1] == '='                       => 2, // !=
            '!' if i + 2 < len && chars[i+1] == '~' && chars[i+2] == '*'  => 3, // !~*
            '!' if i + 1 < len && chars[i+1] == '~'                       => 2, // !~
            '~' if i + 1 < len && chars[i+1] == '*'                       => 2, // ~*
            ':' if i + 1 < len && chars[i+1] == ':'                        => 2, // :: (cast)
            '@' if i + 1 < len && chars[i+1] == '>'                       => 2, // @>
            '<' if i + 1 < len && chars[i+1] == '@'                       => 2, // <@
            '@' if i + 1 < len && chars[i+1] == '@'                       => 2, // @@
            '#' if !hash_is_comment && i + 1 < len && chars[i+1] == '>'   => 2, // #>
            '#' if !hash_is_comment && i + 2 < len && chars[i+1] == '>' && chars[i+2] == '>' => 3, // #>>
            '~' | '=' | '<' | '>' | '+' | '*' | '/' | '%' | '^' | '&'    => 1,
            _ => 0,
        };
        if op_len > 0 {
            let text: String = chars[i..i+op_len].iter().collect();
            tokens.push(Token { text, kind: TokenKind::Operator });
            i += op_len;
        } else {
            tokens.push(Token { text: c.to_string(), kind: TokenKind::Plain });
            i += 1;
        }
    }

    tokens
}

// Keyword / function sets moved to `crate::plane::{mysql,postgres}` — each
// plane owns its own sorted slices and the tokenizer reads them via
// `plane.keywords()` / `plane.functions()`.

/// Find the matching `( )` / `[ ]` / `{ }` for the bracket character at byte
/// offset `pos` in `text`. Returns `Some((open, close))` — both inclusive
/// byte positions — on a match, `None` if `pos` isn't on a bracket or the
/// mate is unbalanced. Used by the shell's double-click handler to extend
/// the textarea selection across a bracket pair span.
///
/// Naive — doesn't skip brackets inside string literals or comments. Good
/// enough for the typical interactive use; can revisit if it confuses.
pub fn find_bracket_match(text: &str, pos: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    if pos >= bytes.len() { return None; }
    let here = bytes[pos] as char;
    let (open_ch, close_ch, forward) = match here {
        '(' => ('(', ')', true),
        '[' => ('[', ']', true),
        '{' => ('{', '}', true),
        ')' => ('(', ')', false),
        ']' => ('[', ']', false),
        '}' => ('{', '}', false),
        _   => return None,
    };
    let mut depth: i32 = 0;
    if forward {
        for (i, &b) in bytes.iter().enumerate().skip(pos) {
            let c = b as char;
            if c == open_ch { depth += 1; }
            else if c == close_ch {
                depth -= 1;
                if depth == 0 { return Some((pos, i)); }
            }
        }
    } else {
        // Walk backwards from `pos`.
        for i in (0..=pos).rev() {
            let c = bytes[i] as char;
            if c == close_ch { depth += 1; }
            else if c == open_ch {
                depth -= 1;
                if depth == 0 { return Some((i, pos)); }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plane::plane_for;
    use shared::DbType;

    fn token_kinds(sql: &str, db: DbType) -> Vec<TokenKind> {
        tokenize_sql(sql, plane_for(db)).into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn test_keyword_major() {
        assert_eq!(token_kinds("SELECT", DbType::Postgres), vec![TokenKind::KeywordMajor]);
    }

    #[test]
    fn test_keyword_minor() {
        // ASC is a keyword but not clause-introducing — stays in the minor bucket.
        assert_eq!(token_kinds("ASC", DbType::Postgres), vec![TokenKind::Keyword]);
    }

    #[test]
    fn test_string_literal() {
        assert_eq!(token_kinds("'hello'", DbType::Postgres), vec![TokenKind::StringLit]);
    }

    #[test]
    fn test_number() {
        assert_eq!(token_kinds("42", DbType::Postgres), vec![TokenKind::NumLit]);
    }

    #[test]
    fn test_operator_arrow() {
        assert_eq!(token_kinds("->", DbType::Postgres), vec![TokenKind::Operator]);
    }

    #[test]
    fn test_operator_double_arrow() {
        assert_eq!(token_kinds("->>", DbType::Postgres), vec![TokenKind::Operator]);
    }

    #[test]
    fn test_operator_cast() {
        assert_eq!(token_kinds("::", DbType::Postgres), vec![TokenKind::Operator]);
    }

    #[test]
    fn test_line_comment() {
        assert_eq!(token_kinds("-- comment", DbType::Postgres), vec![TokenKind::LineComment]);
    }

    #[test]
    fn test_block_comment() {
        assert_eq!(token_kinds("/* comment */", DbType::Postgres), vec![TokenKind::BlockComment]);
    }

    #[test]
    fn test_mixed() {
        let kinds = token_kinds("SELECT 1", DbType::Postgres);
        assert_eq!(kinds, vec![TokenKind::KeywordMajor, TokenKind::Plain, TokenKind::NumLit]);
    }
}
