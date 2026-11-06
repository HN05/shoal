//! Shell text for commands Shoal shows or hands to a shell.

/// Shell text for `words`, quoting each word that is not plainly literal.
pub fn quote<S: AsRef<str>>(words: &[S]) -> String {
    words
        .iter()
        .map(|word| {
            let word = word.as_ref();
            let literal = !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "/._-+:=@%,".contains(c));
            if literal {
                word.to_owned()
            } else {
                format!("'{}'", word.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
