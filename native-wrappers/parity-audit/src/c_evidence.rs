const fn identifier_byte(byte: u8) -> bool {
    // Consume extended identifier spellings conservatively too, so an ASCII
    // suffix of a non-ASCII identifier or compiler extension cannot match.
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || !byte.is_ascii()
}

/// Find a complete code identifier followed by `(`, ignoring lexical trivia.
/// This is a source-name check, not a C declaration parser or preprocessor.
pub fn has_function_identifier(source: &str, symbol: &str) -> bool {
    // C splices escaped newlines before recognizing comments and tokens. Without
    // this step, `renamed_\` followed by a newline and `main` could match `main`.
    let source = source.replace("\\\r\n", "").replace("\\\n", "");
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut follows_symbol = false;
    while index < bytes.len() {
        match bytes[index] {
            byte if byte.is_ascii_whitespace() => index += 1,
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                let Some(end) = bytes[index + 2..].windows(2).position(|pair| pair == b"*/") else {
                    return false;
                };
                index += end + 4;
            }
            quote @ (b'"' | b'\'') => {
                follows_symbol = false;
                index += 1;
                while index < bytes.len() {
                    match bytes[index] {
                        b'\\' => index += 2,
                        byte if byte == quote => {
                            index += 1;
                            break;
                        }
                        _ => index += 1,
                    }
                }
            }
            b'(' if follows_symbol => return true,
            byte if identifier_byte(byte) => {
                let start = index;
                while index < bytes.len() && identifier_byte(bytes[index]) {
                    index += 1;
                }
                follows_symbol = &bytes[start..index] == symbol.as_bytes();
            }
            _ => {
                follows_symbol = false;
                index += 1;
            }
        }
    }
    false
}
