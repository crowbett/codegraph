pub enum Command {
    Set(String, String),
    Get(String),
    Del(String),
    Count,
}

pub struct ParseError {
    pub message: String,
    unknown_verb: bool,
}

impl ParseError {
    pub fn is_fatal(&self) -> bool {
        !self.unknown_verb
    }
}

pub fn parse_line(line: &str) -> Result<Command, ParseError> {
    let words = tokenize(line);
    match words.as_slice() {
        ["set", k, v] => Ok(Command::Set(key(k)?, v.to_string())),
        ["get", k] => Ok(Command::Get(key(k)?)),
        ["del", k] => Ok(Command::Del(key(k)?)),
        ["count"] => Ok(Command::Count),
        _ => Err(fail(line, true)),
    }
}

fn tokenize(line: &str) -> Vec<&str> {
    line.split_whitespace().collect()
}

fn key(word: &str) -> Result<String, ParseError> {
    if word.chars().all(char::is_alphanumeric) {
        Ok(word.to_string())
    } else {
        Err(fail(word, false))
    }
}

fn fail(text: &str, unknown_verb: bool) -> ParseError {
    let message = format!("cannot parse {text:?}");
    ParseError { message, unknown_verb }
}
