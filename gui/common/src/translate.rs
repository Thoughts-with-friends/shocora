//! Machine translation: graphic pack descriptions, and the window's texts in a language nobody
//! wrote a translation for.
//!
//! The service is the user's choice ([`Service`]): Google Translate (the default, no key),
//! an OpenAI-style chat API or a local Ollama, each at an endpoint that can be changed.
//! Only the texts are sent; results are kept for the session (the window's, on disk).
use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex},
    time::Duration,
};

#[path = "languages.rs"]
mod languages;
pub use languages::LANGUAGES;

/// Most characters sent at once: Google refuses requests much longer than 5000.
const BATCH_CHARS: usize = 3000;

/// Who translates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Google,
    OpenAi,
    Ollama,
}

impl Provider {
    pub const ALL: [Self; 3] = [Self::Google, Self::OpenAi, Self::Ollama];

    pub fn label(self) -> &'static str {
        match self {
            Self::Google => "Google Translate",
            Self::OpenAi => "OpenAI",
            Self::Ollama => "Ollama",
        }
    }

    /// Where the service answers unless told otherwise.
    pub fn default_endpoint(self) -> &'static str {
        match self {
            Self::Google => "https://translate.googleapis.com/translate_a/single",
            Self::OpenAi => "https://api.openai.com/v1/chat/completions",
            Self::Ollama => "http://localhost:11434/api/chat",
        }
    }

    /// The model asked for unless told otherwise; empty for services without models.
    pub fn default_model(self) -> &'static str {
        match self {
            Self::Google => "",
            Self::OpenAi => "gpt-4o-mini",
            Self::Ollama => "llama3.2",
        }
    }

    pub fn needs_key(self) -> bool {
        self == Self::OpenAi
    }

    pub fn has_model(self) -> bool {
        matches!(self, Self::OpenAi | Self::Ollama)
    }

    /// Language models are slow; translation services answer at once.
    fn timeout(self) -> Duration {
        Duration::from_secs(if self.has_model() { 180 } else { 20 })
    }
}

/// The translation service and how to reach it, kept in the settings.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Service {
    #[serde(deserialize_with = "provider_or_default")]
    pub provider: Provider,
    pub endpoint: String,
    /// OpenAI's key; unused by the others.
    pub api_key: String,
    /// The language model (OpenAI, Ollama).
    pub model: String,
}

/// A provider no longer offered (DeepL) in older settings becomes the default one, rather
/// than making the whole settings file unreadable.
fn provider_or_default<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Provider, D::Error> {
    use serde::Deserialize;
    let name = String::deserialize(d)?;
    Ok(Provider::deserialize(serde::de::value::StrDeserializer::<serde::de::value::Error>::new(
        &name,
    ))
    .unwrap_or_default())
}

impl Default for Service {
    fn default() -> Self {
        Self::of(Provider::default())
    }
}

impl Service {
    /// `provider` at its default endpoint, with its default model.
    pub fn of(provider: Provider) -> Self {
        Self {
            provider,
            endpoint: provider.default_endpoint().to_owned(),
            api_key: String::new(),
            model: provider.default_model().to_owned(),
        }
    }

    /// Translates `text` into `target` (a Google Translate code), the source language detected.
    /// Lines stay lines.
    ///
    /// # Errors
    /// Fails without a connection, when the service refuses, or on an unexpected reply.
    pub fn translate(&self, text: &str, target: &str) -> Result<String, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .user_agent("shocora")
            .timeout_global(Some(self.provider.timeout()))
            .build()
            .into();
        let endpoint = if self.endpoint.trim().is_empty() {
            self.provider.default_endpoint()
        } else {
            self.endpoint.trim()
        };
        match self.provider {
            Provider::Google => {
                let body = agent
                    .post(endpoint)
                    .query("client", "gtx")
                    .query("sl", "auto")
                    .query("tl", target)
                    .query("dt", "t")
                    .send_form([("q", text)])
                    .map_err(|e| e.to_string())?
                    .body_mut()
                    .read_to_string()
                    .map_err(|e| e.to_string())?;
                parse_google(&body)
            }
            Provider::OpenAi | Provider::Ollama => {
                let messages = serde_json::json!([
                    { "role": "system", "content": instructions(target) },
                    { "role": "user", "content": text },
                ]);
                let model = if self.model.trim().is_empty() {
                    self.provider.default_model()
                } else {
                    self.model.trim()
                };
                if self.provider == Provider::Ollama {
                    let request = serde_json::json!({
                        "model": model,
                        "messages": messages,
                        "stream": false,
                        "options": { "temperature": 0 },
                    });
                    let reply = post_json(&agent, endpoint, None, &request)?;
                    reply["message"]["content"]
                        .as_str()
                        .map(without_thinking)
                        .ok_or_else(|| "unexpected Ollama reply".to_owned())
                } else {
                    let request = serde_json::json!({
                        "model": model,
                        "messages": messages,
                        "temperature": 0,
                    });
                    let key = (!self.api_key.trim().is_empty())
                        .then(|| format!("Bearer {}", self.api_key.trim()));
                    let reply = post_json(&agent, endpoint, key, &request)?;
                    reply["choices"][0]["message"]["content"]
                        .as_str()
                        .map(without_thinking)
                        .ok_or_else(|| "unexpected OpenAI reply".to_owned())
                }
            }
        }
    }
}

/// POSTs `request` as JSON and reads the JSON reply.
fn post_json(
    agent: &ureq::Agent,
    endpoint: &str,
    authorization: Option<String>,
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut post = agent.post(endpoint).header("Content-Type", "application/json");
    if let Some(authorization) = authorization {
        post = post.header("Authorization", authorization);
    }
    let body = post
        .send(request.to_string())
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

/// What a language model is told: the reply must line up with the text, places untouched.
fn instructions(target: &str) -> String {
    let language = language_name(target).unwrap_or(target);
    format!(
        "Translate the user's text into {language}. It is text of a video game emulator's \
         window. Translate line by line: reply with exactly as many lines as the text has, in \
         the same order. Keep placeholders such as {{0}} unchanged. Reply with the \
         translation only, without notes or quotes."
    )
}

/// A reasoning model's reply without its `<think>` part.
fn without_thinking(reply: &str) -> String {
    match reply.split_once("</think>") {
        Some((_, rest)) if reply.trim_start().starts_with("<think>") => rest.trim().to_owned(),
        _ => reply.trim().to_owned(),
    }
}

/// The English name of the language `code`, if Google Translate has it.
pub fn language_name(code: &str) -> Option<&'static str> {
    LANGUAGES.iter().find(|(c, _)| *c == code).map(|(_, name)| *name)
}

/// The `{name}` places of a text.
pub fn places(text: &str) -> BTreeSet<&str> {
    text.split('{').skip(1).filter_map(|rest| rest.split_once('}')).map(|(n, _)| n).collect()
}

/// `text` with its `{name}` places numbered (`{0}`, `{1}`, ...), which a translation leaves
/// alone where it may translate words, and the names in their order.
fn number_places(text: &str) -> (String, Vec<&str>) {
    let names: Vec<&str> = places(text).into_iter().collect();
    let numbered = names.iter().enumerate().fold(text.to_owned(), |text, (i, name)| {
        text.replace(&format!("{{{name}}}"), &format!("{{{i}}}"))
    });
    (numbered, names)
}

/// [`number_places`] undone; `None` unless the translation kept every place.
fn name_places(translated: &str, names: &[&str]) -> Option<String> {
    let expected: BTreeSet<String> = (0..names.len()).map(|i| i.to_string()).collect();
    let found: BTreeSet<String> = places(translated).into_iter().map(str::to_owned).collect();
    if found != expected {
        return None;
    }
    Some(names.iter().enumerate().fold(translated.to_owned(), |text, (i, name)| {
        text.replace(&format!("{{{i}}}"), &format!("{{{name}}}"))
    }))
}

/// Translates each of `texts` into `target`, a few requests for all of them: one-line texts
/// go together, a line each. A text that did not survive (a `{place}` lost, the lines out of
/// step) is `None`.
///
/// # Errors
/// Fails when the service answered none of the requests.
pub fn translate_all(
    service: &Service,
    texts: &[&str],
    target: &str,
) -> Result<Vec<Option<String>>, String> {
    let mut out = vec![None; texts.len()];
    let prepared: Vec<(String, Vec<&str>)> = texts.iter().map(|t| number_places(t)).collect();
    let mut batch: Vec<usize> = Vec::new();
    let mut size = 0;
    let mut results = Vec::new();
    for (i, (text, names)) in prepared.iter().enumerate() {
        if text.trim().is_empty() {
            out[i] = Some(texts[i].to_owned());
        } else if text.contains('\n') {
            let reply = service.translate(text, target);
            results.push(reply.map(|reply| out[i] = name_places(&reply, names)));
        } else {
            if size + text.len() > BATCH_CHARS {
                results.push(translate_lines(service, &batch, &prepared, target, &mut out));
                batch.clear();
                size = 0;
            }
            size += text.len() + 1;
            batch.push(i);
        }
    }
    if !batch.is_empty() {
        results.push(translate_lines(service, &batch, &prepared, target, &mut out));
    }
    if !results.is_empty() && results.iter().all(Result::is_err) {
        return Err(results.into_iter().find_map(Result::err).unwrap_or_default());
    }
    Ok(out)
}

/// The one-line texts `batch` of `prepared` in one request, a line each.
fn translate_lines(
    service: &Service,
    batch: &[usize],
    prepared: &[(String, Vec<&str>)],
    target: &str,
    out: &mut [Option<String>],
) -> Result<(), String> {
    let joined: Vec<&str> = batch.iter().map(|&i| prepared[i].0.as_str()).collect();
    let reply = service.translate(&joined.join("\n"), target)?;
    let lines: Vec<&str> = reply.trim().split('\n').collect();
    if lines.len() == batch.len() {
        for (&i, line) in batch.iter().zip(lines) {
            out[i] = name_places(line.trim(), &prepared[i].1);
        }
    } else {
        // Lines merged or split: one at a time instead.
        for &i in batch {
            if let Ok(line) = service.translate(&prepared[i].0, target) {
                out[i] = name_places(line.trim(), &prepared[i].1);
            }
        }
    }
    Ok(())
}

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

    /// Forgets every translation: the service changed.
    pub fn clear(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }

    /// Starts translating `text` into `target` with `service` on a thread of its own; `done`
    /// is called when it is there (to repaint).
    pub fn request(
        &self,
        service: &Service,
        text: &str,
        target: &str,
        done: impl Fn() + Send + 'static,
    ) {
        let key = (target.to_owned(), text.to_owned());
        {
            let Ok(mut cache) = self.cache.lock() else { return };
            if matches!(cache.get(&key), Some(Translation::Pending | Translation::Done(_))) {
                return;
            }
            cache.insert(key.clone(), Translation::Pending);
        }
        let cache = Arc::clone(&self.cache);
        let service = service.clone();
        std::thread::spawn(move || {
            let result = match service.translate(&key.1, &key.0) {
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

/// Google's reply is `[[["translated", "source", ...], ...], ...]`: one entry per sentence.
fn parse_google(body: &str) -> Result<String, String> {
    let json: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let sentences = json[0].as_array().ok_or("unexpected translation reply")?;
    Ok(sentences.iter().filter_map(|s| s[0].as_str()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: the sentences of a reply are joined in order.
    /// Pass: two sentences give their translations one after the other.
    #[test]
    fn joins_sentences() {
        let reply = r#"[[["解像度を変更します。","Changes the resolution.",null],["\n60FPS","\n60FPS",null]],null,"en"]"#;
        assert_eq!(parse_google(reply).unwrap(), "解像度を変更します。\n60FPS");
    }

    /// Intent: places survive the trip only when the translation keeps their numbers.
    /// Pass: names come back in place; a lost number gives `None`.
    #[test]
    fn places_are_numbered_and_named_back() {
        let (numbered, names) = number_places("{count} codes ({error})");
        assert_eq!(numbered, "{0} codes ({1})");
        assert_eq!(name_places("({1}) {0} Codes", &names).unwrap(), "({error}) {count} Codes");
        assert_eq!(name_places("{0} Codes", &names), None);
    }

    /// Intent: the language list and the services' defaults are sound.
    #[test]
    fn knows_languages_and_services() {
        assert_eq!(language_name("ja"), Some("Japanese"));
        assert!(LANGUAGES.len() > 200);
        assert_eq!(Service::default().provider, Provider::Google);
        assert_eq!(without_thinking("<think>hm</think>\nHallo"), "Hallo");
        let service: Service = toml::from_str("provider = \"ollama\"").unwrap();
        assert_eq!(service.provider, Provider::Ollama);
        let service: Service = toml::from_str("provider = \"deepl\"").unwrap();
        assert_eq!(service.provider, Provider::Google);
    }

    /// Intent: a batch comes back one text per line (network).
    #[test]
    #[ignore = "uses the network"]
    fn translates_a_batch() {
        let texts = ["File", "Open {count} files", "Settings\nGeneral", "Quit"];
        let out = translate_all(&Service::default(), &texts, "de").unwrap();
        println!("{out:?}");
        assert!(out.iter().all(Option::is_some));
        assert!(out[1].as_ref().unwrap().contains("{count}"));
    }
}
