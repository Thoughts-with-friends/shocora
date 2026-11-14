//! `meta/meta.xml`, `code/app.xml` and `code/cos.xml`.
use std::path::Path;

use crate::TitleError;

/// Fields of interest from the title's XML files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TitleMeta {
    pub title_id: Option<u64>,
    pub title_version: Option<u32>,
    /// English name (`longname_en`).
    pub long_name: Option<String>,
    /// Japanese name (`longname_ja`).
    pub long_name_ja: Option<String>,
    /// RPX file name from `cos.xml`'s `argstr`.
    pub rpx_name: Option<String>,
}

fn child_text<'a>(doc: &'a roxmltree::Document<'_>, tag: &str) -> Option<&'a str> {
    doc.descendants().find(|n| n.has_tag_name(tag)).and_then(|n| n.text()).map(str::trim)
}

fn parse_hex(text: &str) -> Option<u64> {
    u64::from_str_radix(text.trim_start_matches("0x"), 16).ok()
}

fn parse<'a>(text: &'a str, path: &Path) -> Result<roxmltree::Document<'a>, TitleError> {
    roxmltree::Document::parse(text)
        .map_err(|e| TitleError::Xml { path: path.to_owned(), message: e.to_string() })
}

impl TitleMeta {
    /// Merges `meta.xml` contents.
    ///
    /// # Errors
    /// Fails on malformed XML.
    pub fn apply_meta_xml(&mut self, text: &str, path: &Path) -> Result<(), TitleError> {
        let doc = parse(text, path)?;
        self.title_id = self.title_id.or_else(|| child_text(&doc, "title_id").and_then(parse_hex));

        let name =
            |tag| child_text(&doc, tag).filter(|s| !s.is_empty()).map(|s| s.replace('\n', " "));
        self.long_name = name("longname_en").or_else(|| self.long_name.take());
        self.long_name_ja = name("longname_ja").or_else(|| self.long_name_ja.take());

        Ok(())
    }

    /// Merges `app.xml` contents.
    ///
    /// # Errors
    /// Fails on malformed XML.
    #[allow(clippy::cast_possible_truncation)]
    pub fn apply_app_xml(&mut self, text: &str, path: &Path) -> Result<(), TitleError> {
        let doc = parse(text, path)?;
        self.title_id = child_text(&doc, "title_id").and_then(parse_hex).or(self.title_id);
        self.title_version =
            child_text(&doc, "title_version").and_then(parse_hex).map(|v| v as u32);
        Ok(())
    }

    /// Merges `cos.xml` contents.
    ///
    /// # Errors
    /// Fails on malformed XML.
    pub fn apply_cos_xml(&mut self, text: &str, path: &Path) -> Result<(), TitleError> {
        let doc = parse(text, path)?;
        self.rpx_name = child_text(&doc, "argstr").filter(|s| !s.is_empty()).map(str::to_owned);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xml_files() {
        let mut meta = TitleMeta::default();
        let path = Path::new("x");

        meta.apply_app_xml(
            r#"<?xml version="1.0"?><app type="complex"><title_id type="hexBinary" length="8">00050000DEAD0100</title_id><title_version type="hexBinary" length="2">0010</title_version></app>"#,
            path,
        )
        .unwrap();
        meta.apply_cos_xml(
            r#"<app><argstr type="string" length="4096">game.rpx</argstr></app>"#,
            path,
        )
        .unwrap();
        meta.apply_meta_xml(
            r#"<menu><longname_en type="string">Demo
Title</longname_en><longname_ja type="string">デモ</longname_ja></menu>"#,
            path,
        )
        .unwrap();

        assert_eq!(meta.title_id, Some(0x0005_0000_DEAD_0100));
        assert_eq!(meta.title_version, Some(16));
        assert_eq!(meta.rpx_name.as_deref(), Some("game.rpx"));
        assert_eq!(meta.long_name.as_deref(), Some("Demo Title"));
        assert_eq!(meta.long_name_ja.as_deref(), Some("デモ"));
    }
}

/// Tests against the developer's own title (`shocora.toml`); they pass without one.
#[cfg(test)]
mod rom_tests {
    use wiiu_test_roms::{report, require};

    use super::TitleMeta;

    /// Intent: the three XML files of an installed title give the identity the emulator
    /// boots with: title id and version (`meta.xml`), and the executable (`cos.xml`).
    /// Pass: every file parses; the title id equals the one in `meta.xml` as read by the
    /// test helper; the `.rpx` that `cos.xml` names exists in `code/`.
    #[test]
    fn reads_the_identity_of_an_installed_title() {
        let title = require!(wiiu_test_roms::title(), "no title");
        let mut meta = TitleMeta::default();
        for (file, apply) in [
            ("meta/meta.xml", TitleMeta::apply_meta_xml as fn(&mut TitleMeta, &str, &_) -> _),
            ("code/app.xml", TitleMeta::apply_app_xml),
            ("code/cos.xml", TitleMeta::apply_cos_xml),
        ] {
            let path = title.join(file);
            let text = std::fs::read_to_string(&path).unwrap();
            apply(&mut meta, &text, &path).unwrap();
        }
        let id = meta.title_id.unwrap();
        assert_eq!(Some(id), wiiu_test_roms::title_id(&title));
        let rpx = meta.rpx_name.clone().unwrap();
        assert!(title.join("code").join(&rpx).is_file(), "{rpx} is in code/");
        report("title id", format!("{id:016x}"));
        report("title version", format!("{:?}", meta.title_version));
        report("name (en)", format!("{:?}", meta.long_name));
        report("name (ja)", format!("{:?}", meta.long_name_ja));
        report("rpx (cos.xml argstr)", rpx);
    }
}
