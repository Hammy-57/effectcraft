//! Conservative static references for freeze dependency capture. Unknown JavaScript falls
//! back to live rendering; it never authorizes an incomplete dependency set.

#[derive(Debug, PartialEq)]
enum Token {
    Name(String),
    Text(String),
    Punct(char),
}

/// `(is_comp, name)`. None means dynamic/opaque lookups cannot be verified safely.
pub fn references(source: &str) -> Option<Vec<(bool, String)>> {
    if source.len() > 100_000 {
        return None;
    }
    let mut chars = source.chars().peekable();
    let mut tokens = vec![];
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut closed = false;
            while let Some(c) = chars.next() {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    closed = true;
                    break;
                }
            }
            if !closed {
                return None;
            }
            continue;
        }
        if c == '\'' || c == '"' {
            let mut text = String::new();
            let mut closed = false;
            while let Some(n) = chars.next() {
                if n == c {
                    closed = true;
                    break;
                }
                if n == '\\' {
                    text.push(match chars.next()? {
                        '\\' => '\\',
                        '\'' => '\'',
                        '"' => '"',
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        _ => return None,
                    });
                } else {
                    text.push(n);
                }
            }
            if !closed {
                return None;
            }
            tokens.push(Token::Text(text));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' || c == '$' {
            let mut name = c.to_string();
            while chars.peek().is_some_and(|n| n.is_ascii_alphanumeric() || *n == '_' || *n == '$') {
                if let Some(n) = chars.next() {
                    name.push(n);
                }
            }
            if name.starts_with("__")
                || matches!(
                    name.as_str(),
                    "eval"
                        | "Function"
                        | "Reflect"
                        | "Object"
                        | "constructor"
                        | "globalThis"
                        | "this"
                        | "global"
                        | "window"
                        | "self"
                        | "Date"
                        | "thisProject"
                        | "app"
                )
            {
                return None;
            }
            tokens.push(Token::Name(name));
        } else {
            if c == '`' || c == '\\' || !c.is_ascii() {
                return None;
            }
            tokens.push(Token::Punct(c));
        }
    }
    let mut refs = vec![];
    for (index, token) in tokens.iter().enumerate() {
        if matches!(token,Token::Name(n) if n=="random")
            && index >= 2
            && tokens.get(index - 1) == Some(&Token::Punct('.'))
            && matches!(tokens.get(index-2),Some(Token::Name(n)) if n=="Math")
        {
            return None;
        }
        if let Token::Name(name) = token
            && matches!(name.as_str(), "comp" | "footage")
        {
            match (tokens.get(index.saturating_add(1)), tokens.get(index.saturating_add(2)), tokens.get(index.saturating_add(3))) {
                (Some(Token::Punct('(')), Some(Token::Text(value)), Some(Token::Punct(')'))) => refs.push((name == "comp", value.clone())),
                _ => return None,
            }
        }
    }
    Some(refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_links_are_scoped_but_aliases_dynamic_and_eval_are_not() {
        assert_eq!(references("comp('Controls').layer('CTRL').effect('Color')('Color')"), Some(vec![(true, "Controls".into())]));
        assert_eq!(references("wiggle(2,10) // comp('ignored')"), Some(vec![]));
        assert_eq!(references("footage(/* note */ \"Data\").dataValue([0,1])"), Some(vec![(false, "Data".into())]));
        for code in
            ["comp(name)", "var other=comp; other('Controls')", "eval('comp(\"Controls\")')", "this['comp']('Controls')", "comp(`Controls`)", "comp('A'+'B')"]
        {
            assert_eq!(references(code), None, "{code}");
        }
    }
}
