//! Machine translation of graphic pack descriptions through Google Translate.
//!
//! Only text the user chose to translate is sent; results are kept for the session.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

const ENDPOINT: &str = "https://translate.googleapis.com/translate_a/single";

/// Where a translation stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Translation {
    Pending,
    Done(String),
    Failed(String),
}

/// Translations made or under way, by target language and source text.
#[derive(Debug, Clone, Default)]
pub struct Translator {
    cache: Arc<Mutex<HashMap<(String, String), Translation>>>,
}

impl Translator {
    /// The translation of `text` into `target` (`ja`, `de`, ...), if asked for before.
    pub fn get(&self, text: &str, target: &str) -> Option<Translation> {
        let cache = self.cache.lock().ok()?;
        cache.get(&(target.to_owned(), text.to_owned())).cloned()
    }

    /// Starts translating `text` into `target` on a thread of its own; `done` is called when
    /// it is there (to repaint).
    pub fn request(&self, text: &str, target: &str, done: impl Fn() + Send + 'static) {
        let key = (target.to_owned(), text.to_owned());
        {
            let Ok(mut cache) = self.cache.lock() else { return };
            if matches!(cache.get(&key), Some(Translation::Pending | Translation::Done(_))) {
                return;
            }
            cache.insert(key.clone(), Translation::Pending);
        }
        let cache = Arc::clone(&self.cache);
        std::thread::spawn(move || {
            let result = match translate(&key.1, &key.0) {
                Ok(text) => Translation::Done(text),
                Err(error) => Translation::Failed(error),
            };
            if let Ok(mut cache) = cache.lock() {
                cache.insert(key, result);
            }
            done();
        });
    }
}

/// Translates `text` into `target`, the source language detected.
///
/// # Errors
/// Fails without a connection or on an unexpected reply.
pub fn translate(text: &str, target: &str) -> Result<String, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .user_agent("shocora")
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build()
        .into();
    let body = agent
        .post(ENDPOINT)
        .query("client", "gtx")
        .query("sl", "auto")
        .query("tl", target)
        .query("dt", "t")
        .send_form([("q", text)])
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    parse_reply(&body)
}

/// The reply is `[[["translated", "source", ...], ...], ...]`: one entry per sentence.
fn parse_reply(body: &str) -> Result<String, String> {
    let json: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let sentences = json[0].as_array().ok_or("unexpected translation reply")?;
    Ok(sentences.iter().filter_map(|s| s[0].as_str()).collect())
}

#[cfg(test)]
mod tests {
    /// Intent: the sentences of a reply are joined in order.
    /// Pass: two sentences give their translations one after the other.
    #[test]
    fn joins_sentences() {
        let reply = r#"[[["解像度を変更します。","Changes the resolution.",null],["\n60FPS","\n60FPS",null]],null,"en"]"#;
        assert_eq!(super::parse_reply(reply).unwrap(), "解像度を変更します。\n60FPS");
    }

    /// Intent: the real service answers (network).
    #[test]
    #[ignore = "uses the network"]
    fn translates_with_the_service() {
        let text = super::translate("Changes the rendering resolution.", "ja").unwrap();
        println!("{text}");
        assert!(!text.is_empty());
    }
}
