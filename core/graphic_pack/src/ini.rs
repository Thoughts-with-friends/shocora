//! Port of Cemu's `IniParser`: the dialect of `rules.txt`.
//!
//! `[Section]` headers, `name = value` options (names compared case-insensitively),
//! comments from `#` or `;` outside quotes, and values in quotes unwrapped. Sections may
//! repeat (`[Preset]`, `[TextureRedefine]`) and keep their order.

/// One `[name]` section and its options in file order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub line: usize,
    pub options: Vec<(String, String)>,
}

impl Section {
    /// The value of option `name` (ASCII case-insensitive), the first if repeated.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.options.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// Whether this is section `name` (case-insensitive).
    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }
}

/// Parses `text` into sections. Malformed lines are skipped, as Cemu does silently.
pub fn parse(text: &str) -> Vec<Section> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut sections: Vec<Section> = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line_number = index + 1;
        let mut line = raw.trim_start_matches([' ', '\t']);
        // Cut comments outside quotes.
        let mut in_quote = false;
        for (i, c) in line.char_indices() {
            if c == '"' {
                in_quote = !in_quote;
            }
            if (c == '#' || c == ';') && !in_quote {
                line = &line[..i];
                break;
            }
        }
        if line.trim().is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let name = rest.find(']').map_or(rest, |end| &rest[..end]);
            sections.push(Section {
                name: name.to_owned(),
                line: line_number,
                options: Vec::new(),
            });
            continue;
        }
        let Some((name, value)) = line.split_once('=') else { continue };
        let name = name.trim_matches([' ', '\t']);
        let mut value = value.trim_matches([' ', '\t', '\r']);
        if name.is_empty()
            || name.bytes().any(|c| c == b'[' || c == b']' || !(32..=128).contains(&c) || c == b' ')
        {
            continue;
        }
        if let Some(rest) = value.strip_prefix('"') {
            match rest.strip_suffix('"') {
                Some(inner) if rest.len() >= 2 => value = inner,
                _ => continue,
            }
        }
        if let Some(section) = sections.last_mut() {
            section.options.push((name.to_owned(), value.to_owned()));
        }
    }
    sections
}

#[cfg(test)]
mod tests {
    /// Intent: rules.txt syntax is read as Cemu's `IniParser` reads it.
    /// Pass: comments are cut except in quotes, quotes are removed, names match in any case,
    /// repeated sections stay separate.
    #[test]
    fn reads_rules_syntax() {
        let sections = super::parse(
            "\u{feff}[Definition]\ntitleIds = 0005000010116100 # XCX\npath = \"A/B # not a comment\"\n\
             \n[Preset]\nname = One\n$width = 1280\n[Preset]\nNAME = Two ; comment\n",
        );
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].get("titleids"), Some("0005000010116100"));
        assert_eq!(sections[0].get("path"), Some("A/B # not a comment"));
        assert_eq!(sections[1].get("$width"), Some("1280"));
        assert_eq!(sections[2].get("name"), Some("Two"));
        assert!(sections[2].is("preset"));
    }
}
