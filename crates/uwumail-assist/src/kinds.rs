//! The kinds of provider and what each one needs: where it is, how it is asked (the API's shape), a
//! key or not, and the models to start with.

use serde::Serialize;

/// How a provider is spoken to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// OpenAI's Chat Completions, which most providers offer as well.
    Chat,
    /// Anthropic's Messages API.
    Anthropic,
    /// The Responses API behind a ChatGPT subscription (the Codex backend).
    Codex,
}

/// Where a kind's address comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BaseUrl {
    /// Always the same; nothing to enter.
    Fixed,
    /// The default unless another is given (an EU endpoint, a proxy).
    Optional,
    /// Has to be given (a server of one's own).
    Required,
}

/// Whether a kind needs a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Key {
    Required,
    Optional,
    None,
    /// Signed in instead (ChatGPT).
    Login,
}

/// How a kind asks for `max_tokens`, and what else it understands, for Chat Completions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatFlavor {
    /// `max_completion_tokens` instead of `max_tokens` (OpenAI's reasoning models want it).
    pub max_completion_tokens: bool,
    /// Sends the usage at the end of a stream when asked with `stream_options`.
    pub stream_usage: bool,
}

/// A kind of provider, as the admin panel and the settings offer it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KindInfo {
    pub kind: &'static str,
    pub name: &'static str,
    pub default_base_url: Option<&'static str>,
    pub base_url: BaseUrl,
    pub key: Key,
    pub model: Option<&'static str>,
    pub fast_model: Option<&'static str>,
    /// Where to get a key.
    pub key_url: Option<&'static str>,
    pub experimental: bool,
    /// Only a person may add it for themselves, not the admin for everyone.
    pub personal_only: bool,
    #[serde(skip)]
    pub shape: Shape,
    #[serde(skip)]
    pub flavor: ChatFlavor,
    /// Models to offer when the provider has no list.
    #[serde(skip)]
    pub known_models: &'static [&'static str],
}

const PLAIN: ChatFlavor = ChatFlavor { max_completion_tokens: false, stream_usage: false };

pub const KINDS: &[KindInfo] = &[
    KindInfo {
        kind: "openai",
        name: "OpenAI",
        default_base_url: Some("https://api.openai.com/v1"),
        base_url: BaseUrl::Optional,
        key: Key::Required,
        model: Some("gpt-5-mini"),
        fast_model: Some("gpt-5-nano"),
        key_url: Some("https://platform.openai.com/api-keys"),
        experimental: false,
        personal_only: false,
        shape: Shape::Chat,
        flavor: ChatFlavor { max_completion_tokens: true, stream_usage: true },
        known_models: &[],
    },
    KindInfo {
        kind: "anthropic",
        name: "Anthropic Claude",
        default_base_url: Some("https://api.anthropic.com/v1"),
        base_url: BaseUrl::Optional,
        key: Key::Required,
        model: Some("claude-sonnet-5"),
        fast_model: Some("claude-haiku-4-5"),
        key_url: Some("https://console.anthropic.com/settings/keys"),
        experimental: false,
        personal_only: false,
        shape: Shape::Anthropic,
        flavor: PLAIN,
        known_models: &[],
    },
    KindInfo {
        kind: "gemini",
        name: "Google Gemini",
        default_base_url: Some("https://generativelanguage.googleapis.com/v1beta/openai"),
        base_url: BaseUrl::Fixed,
        key: Key::Required,
        model: Some("gemini-2.5-flash"),
        fast_model: Some("gemini-2.5-flash-lite"),
        key_url: Some("https://aistudio.google.com/apikey"),
        experimental: false,
        personal_only: false,
        shape: Shape::Chat,
        flavor: PLAIN,
        known_models: &[],
    },
    KindInfo {
        kind: "mistral",
        name: "Mistral",
        default_base_url: Some("https://api.mistral.ai/v1"),
        base_url: BaseUrl::Fixed,
        key: Key::Required,
        model: Some("mistral-medium-latest"),
        fast_model: Some("mistral-small-latest"),
        key_url: Some("https://console.mistral.ai/api-keys"),
        experimental: false,
        personal_only: false,
        shape: Shape::Chat,
        flavor: PLAIN,
        known_models: &[],
    },
    KindInfo {
        kind: "openrouter",
        name: "OpenRouter",
        default_base_url: Some("https://openrouter.ai/api/v1"),
        base_url: BaseUrl::Fixed,
        key: Key::Required,
        model: Some("openai/gpt-5-mini"),
        fast_model: Some("google/gemini-2.5-flash-lite"),
        key_url: Some("https://openrouter.ai/settings/keys"),
        experimental: false,
        personal_only: false,
        shape: Shape::Chat,
        flavor: ChatFlavor { max_completion_tokens: false, stream_usage: true },
        known_models: &[],
    },
    KindInfo {
        kind: "ollama",
        name: "Ollama",
        default_base_url: None,
        base_url: BaseUrl::Required,
        key: Key::None,
        model: None,
        fast_model: None,
        key_url: None,
        experimental: false,
        personal_only: false,
        shape: Shape::Chat,
        flavor: ChatFlavor { max_completion_tokens: false, stream_usage: true },
        known_models: &[],
    },
    KindInfo {
        kind: "openaiCompatible",
        name: "OpenAI-compatible",
        default_base_url: None,
        base_url: BaseUrl::Required,
        key: Key::Optional,
        model: None,
        fast_model: None,
        key_url: None,
        experimental: false,
        personal_only: false,
        shape: Shape::Chat,
        flavor: PLAIN,
        known_models: &[],
    },
    KindInfo {
        kind: "chatgpt",
        name: "ChatGPT (subscription)",
        default_base_url: Some("https://chatgpt.com/backend-api/codex"),
        base_url: BaseUrl::Fixed,
        key: Key::Login,
        model: Some("gpt-5.1"),
        fast_model: Some("gpt-5.1-codex-mini"),
        key_url: None,
        experimental: true,
        personal_only: true,
        shape: Shape::Codex,
        flavor: PLAIN,
        known_models: &["gpt-5.1", "gpt-5.1-codex", "gpt-5.1-codex-max", "gpt-5.1-codex-mini"],
    },
];

pub fn kind(name: &str) -> Option<&'static KindInfo> {
    KINDS.iter().find(|info| info.kind == name)
}

/// The address requests go to: the stored one for kinds that take one, the kind's otherwise. Ollama's
/// OpenAI-compatible API lives under `/v1`, which people usually leave out.
pub fn endpoint(info: &KindInfo, stored: Option<&str>) -> Option<String> {
    let base = match (info.base_url, stored.map(str::trim).filter(|url| !url.is_empty())) {
        (BaseUrl::Fixed, _) | (BaseUrl::Optional, None) => info.default_base_url?.to_owned(),
        (_, Some(url)) => url.to_owned(),
        (BaseUrl::Required, None) => return None,
    };
    let base = base.trim_end_matches('/').to_owned();
    if info.kind == "ollama" && !base.ends_with("/v1") {
        return Some(format!("{base}/v1"));
    }
    Some(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints() {
        let ollama = kind("ollama").unwrap();
        assert_eq!(endpoint(ollama, Some("http://192.0.2.10:11434/")).unwrap(), "http://192.0.2.10:11434/v1");
        assert_eq!(endpoint(ollama, Some("http://192.0.2.10:11434/v1")).unwrap(), "http://192.0.2.10:11434/v1");
        assert_eq!(endpoint(ollama, None), None);
        let gemini = kind("gemini").unwrap();
        assert_eq!(
            endpoint(gemini, Some("https://evil.example")).unwrap(),
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "a fixed address can't be changed"
        );
        let openai = kind("openai").unwrap();
        assert_eq!(endpoint(openai, None).unwrap(), "https://api.openai.com/v1");
        assert_eq!(endpoint(openai, Some("https://eu.api.openai.com/v1")).unwrap(), "https://eu.api.openai.com/v1");
        assert!(kind("chatgpt").unwrap().personal_only);
        assert!(kind("claude-subscription").is_none());
    }
}
