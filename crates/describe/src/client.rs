//! One OpenAI-compatible chat client serving all three backends, with each
//! backend's dialect quirks kept explicit and pinned by tests.

use base64::Engine as _;
use majestical_core::ports::{Caption, Describer, PortError, TagSubject, TagSuggestion};

use crate::config::{BackendKind, DescriberConfig};

const CONNECT_TIMEOUT_SECS: u64 = 10;
const REQUEST_TIMEOUT_SECS: u64 = 120;
const MALFORMED_SNIPPET_LEN: usize = 120;

const CAPTION_PROMPT: &str = "Describe this image in one concise sentence for a media \
    catalog. Reply with only the caption, no preamble.";

fn tags_prompt(vocab: &[String]) -> String {
    let vocab_list = if vocab.is_empty() {
        "(none yet)".to_string()
    } else {
        vocab.join(", ")
    };
    format!(
        "Suggest tags for this media. Existing catalog tags: {vocab_list}. Prefer \
         existing tags when they apply; propose new lowercase tags only when clearly \
         warranted. Reply with ONLY this JSON, no other text: \
         {{\"tags\":[{{\"tag\":\"...\",\"confidence\":0.0}}]}}"
    )
}

fn to_port_error(context: impl Into<String>, error: &DescribeHttpError) -> PortError {
    PortError::new(context, std::io::Error::other(error.to_string()))
}

/// All three backends accept base64 data URLs on the OpenAI-compatible
/// endpoint; Ollama accepts ONLY data URLs (never http URLs), which is why
/// data URLs are the one shared dialect.
fn image_content(image: &[u8], prompt: &str) -> serde_json::Value {
    let encoded = base64::engine::general_purpose::STANDARD.encode(image);
    serde_json::json!([
        {"type": "text", "text": prompt},
        {"type": "image_url", "image_url": {"url": format!("data:image/webp;base64,{encoded}")}},
    ])
}

#[derive(Debug, thiserror::Error)]
enum DescribeHttpError {
    #[error("request to {url}: {message}")]
    Request { url: String, message: String },
    #[error("backend returned malformed JSON after retry: {snippet}")]
    Malformed { snippet: String },
    #[error("backend response missing choices[0].message.content")]
    Shape,
}

/// Result of `maj describer test`'s live probe.
#[derive(Debug)]
pub struct ProbeReport {
    pub reachable: bool,
    pub model_listed: bool,
    /// LM Studio only: whether the configured model reports vision support.
    pub vision: Option<bool>,
}

pub struct HttpDescriber {
    config: DescriberConfig,
    api_key: Option<String>,
    agent: ureq::Agent,
}

impl HttpDescriber {
    /// `env_key` is the caller-read `MAJ_OPENROUTER_KEY` (this crate never
    /// touches the environment; the CLI reads it).
    #[must_use]
    pub fn new(config: DescriberConfig, env_key: Option<String>) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(std::time::Duration::from_secs(CONNECT_TIMEOUT_SECS)))
            .timeout_global(Some(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS)))
            .build()
            .into();
        let api_key = config.effective_api_key(env_key);
        Self {
            config,
            api_key,
            agent,
        }
    }

    fn chat_url(&self) -> String {
        format!(
            "{}/v1/chat/completions",
            self.config.base_url.trim_end_matches('/')
        )
    }

    fn authorize<S>(&self, req: ureq::RequestBuilder<S>) -> ureq::RequestBuilder<S> {
        match &self.api_key {
            Some(key) => req.header("Authorization", format!("Bearer {key}")),
            None => req,
        }
    }

    fn post_chat(&self, content: &serde_json::Value) -> Result<String, DescribeHttpError> {
        let url = self.chat_url();
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": [{"role": "user", "content": content}],
        });
        let request = self.authorize(self.agent.post(&url));
        let error_at = |message: String| DescribeHttpError::Request {
            url: url.clone(),
            message,
        };
        let mut response = request
            .send_json(body)
            .map_err(|error| error_at(error.to_string()))?;
        let value: serde_json::Value = response
            .body_mut()
            .read_json()
            .map_err(|error| error_at(error.to_string()))?;
        value["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or(DescribeHttpError::Shape)
    }

    fn parse_tags(&self, content: &str, vocab: &[String]) -> Option<Vec<TagSuggestion>> {
        #[derive(serde::Deserialize)]
        struct RawTag {
            tag: String,
            confidence: f64,
        }
        #[derive(serde::Deserialize)]
        struct RawTags {
            tags: Vec<RawTag>,
        }
        let parsed: RawTags = serde_json::from_str(content).ok()?;
        let suggestions = parsed
            .tags
            .into_iter()
            .map(|raw| TagSuggestion {
                in_vocab: vocab.iter().any(|entry| entry == &raw.tag),
                confidence: raw.confidence.clamp(0.0, 1.0),
                tag: raw.tag,
                model_tag: self.config.model_tag(),
            })
            .collect();
        Some(suggestions)
    }

    fn tags_once(
        &self,
        subject: &TagSubject<'_>,
        prompt: &str,
    ) -> Result<String, DescribeHttpError> {
        let content = match subject {
            TagSubject::Image(image) => image_content(image, prompt),
            TagSubject::Captions(captions) => serde_json::Value::String(format!(
                "{prompt}\n\nKeyframe captions:\n{}",
                captions.join("\n")
            )),
        };
        self.post_chat(&content)
    }

    /// Live probe used by `maj describer test`.
    ///
    /// # Errors
    /// Returns `PortError` when the backend cannot be reached at all.
    pub fn probe(&self) -> Result<ProbeReport, PortError> {
        let url = format!("{}/v1/models", self.config.base_url.trim_end_matches('/'));
        let request = self.authorize(self.agent.get(&url));
        let error_at = |message: String| {
            PortError::new(format!("probe {url}"), std::io::Error::other(message))
        };
        let mut response = request
            .call()
            .map_err(|error| error_at(error.to_string()))?;
        let body: serde_json::Value = response
            .body_mut()
            .read_json()
            .map_err(|error| error_at(error.to_string()))?;
        let model_listed = body["data"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["id"] == self.config.model));
        let vision = if matches!(self.config.backend, BackendKind::LmStudio) {
            self.lm_studio_vision(&self.config.base_url)
        } else {
            None
        };
        Ok(ProbeReport {
            reachable: true,
            model_listed,
            vision,
        })
    }

    fn lm_studio_vision(&self, base: &str) -> Option<bool> {
        let url = format!("{}/api/v1/models", base.trim_end_matches('/'));
        let request = self.authorize(self.agent.get(&url));
        let mut response = request.call().ok()?;
        let body: serde_json::Value = response.body_mut().read_json().ok()?;
        let entry = body["data"]
            .as_array()?
            .iter()
            .find(|item| item["id"] == self.config.model)?;
        entry["capabilities"]["vision"].as_bool()
    }
}

impl Describer for HttpDescriber {
    fn caption(&self, image: &[u8]) -> Result<Caption, PortError> {
        let content = image_content(image, CAPTION_PROMPT);
        let text = self
            .post_chat(&content)
            .map_err(|error| to_port_error("caption", &error))?;
        Ok(Caption {
            text: text.trim().to_string(),
            model_tag: self.config.model_tag(),
        })
    }

    fn suggest_tags(
        &self,
        subject: TagSubject<'_>,
        existing_vocab: &[String],
    ) -> Result<Vec<TagSuggestion>, PortError> {
        let prompt = tags_prompt(existing_vocab);
        let content = self
            .tags_once(&subject, &prompt)
            .map_err(|error| to_port_error("suggest_tags", &error))?;
        if let Some(suggestions) = self.parse_tags(&content, existing_vocab) {
            return Ok(suggestions);
        }

        let retry_prompt = format!("{prompt} Reply with ONLY the JSON object.");
        let retry_content = self
            .tags_once(&subject, &retry_prompt)
            .map_err(|error| to_port_error("suggest_tags retry", &error))?;
        if let Some(suggestions) = self.parse_tags(&retry_content, existing_vocab) {
            return Ok(suggestions);
        }

        let snippet: String = retry_content.chars().take(MALFORMED_SNIPPET_LEN).collect();
        Err(to_port_error(
            "suggest_tags",
            &DescribeHttpError::Malformed { snippet },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BackendKind, DescriberConfig};
    use httpmock::prelude::*;
    use majestical_core::ports::{Describer, TagSubject};

    fn config_for(server: &MockServer, backend: BackendKind, key: Option<&str>) -> DescriberConfig {
        DescriberConfig {
            backend,
            base_url: server.base_url(),
            model: "test-model".into(),
            api_key: key.map(str::to_string),
        }
    }

    fn caption_body() -> serde_json::Value {
        serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "a red barn at dusk"}}]
        })
    }

    #[test]
    fn ollama_caption_sends_base64_data_url_no_auth() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions")
                .header_missing("authorization")
                .is_true(|req: &HttpMockRequest| {
                    let body: serde_json::Value =
                        serde_json::from_slice(&req.body_bytes()).expect("json body");
                    let Some(content) = body["messages"][0]["content"].as_array() else {
                        return false;
                    };
                    let Some(url) = content[1]["image_url"]["url"].as_str() else {
                        return false;
                    };
                    body["model"] == "test-model" && url.starts_with("data:image/webp;base64,")
                });
            then.status(200).json_body(caption_body());
        });

        let config = config_for(&server, BackendKind::Ollama, None);
        let describer = HttpDescriber::new(config, None);
        let caption = describer.caption(b"fake-image-bytes").expect("caption");

        assert_eq!(caption.text, "a red barn at dusk");
        assert_eq!(caption.model_tag, "describe-test-model");
        mock.assert_calls(1);
    }

    #[test]
    fn openrouter_sends_bearer_auth() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions")
                .header("authorization", "Bearer sk-test");
            then.status(200).json_body(caption_body());
        });

        let config = config_for(&server, BackendKind::OpenRouter, Some("sk-test"));
        let describer = HttpDescriber::new(config, None);
        let caption = describer.caption(b"fake-image-bytes").expect("caption");

        assert_eq!(caption.text, "a red barn at dusk");
        mock.assert_calls(1);
    }

    #[test]
    fn suggest_tags_parses_strict_json_and_marks_vocab() {
        let server = MockServer::start();
        let response = serde_json::json!({
            "tags": [
                {"tag": "person/dana", "confidence": 0.9},
                {"tag": "barn", "confidence": 0.6}
            ]
        });
        let content = serde_json::to_string(&response).expect("serialize");
        let mock = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": content}}]
            }));
        });

        let config = config_for(&server, BackendKind::Ollama, None);
        let describer = HttpDescriber::new(config, None);
        let vocab = vec!["person/dana".to_string(), "status/select".to_string()];
        let suggestions = describer
            .suggest_tags(TagSubject::Image(b"fake-image-bytes"), &vocab)
            .expect("suggest_tags");

        assert_eq!(suggestions.len(), 2);
        assert!(suggestions[0].in_vocab);
        assert!(!suggestions[1].in_vocab);
        mock.assert_calls(1);
    }

    #[test]
    fn suggest_tags_retries_once_on_malformed_json() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": "Sure! Here are tags: barn"}}]
            }));
        });

        let config = config_for(&server, BackendKind::Ollama, None);
        let describer = HttpDescriber::new(config, None);
        let result = describer.suggest_tags(TagSubject::Image(b"fake-image-bytes"), &[]);

        assert!(result.is_err());
        mock.assert_calls(2);
    }

    #[test]
    fn captions_subject_is_text_only_no_image_part() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions")
                .is_true(|req: &HttpMockRequest| {
                    let body: serde_json::Value =
                        serde_json::from_slice(&req.body_bytes()).expect("json body");
                    body["messages"][0]["content"].is_string()
                });
            then.status(200).json_body(serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": "{\"tags\":[]}"}}]
            }));
        });

        let config = config_for(&server, BackendKind::Ollama, None);
        let describer = HttpDescriber::new(config, None);
        let captions = vec!["a barn".to_string(), "at dusk".to_string()];
        let suggestions = describer
            .suggest_tags(TagSubject::Captions(&captions), &[])
            .expect("suggest_tags");

        assert!(suggestions.is_empty());
        mock.assert_calls(1);
    }

    #[test]
    fn probe_reports_lm_studio_vision_capability() {
        let server = MockServer::start();
        let models_mock = server.mock(|when, then| {
            when.method(GET).path("/v1/models");
            then.status(200)
                .json_body(serde_json::json!({"data": [{"id": "test-model"}]}));
        });
        let lm_studio_mock = server.mock(|when, then| {
            when.method(GET).path("/api/v1/models");
            then.status(200).json_body(serde_json::json!({
                "data": [{"id": "test-model", "type": "llm", "capabilities": {"vision": false}}]
            }));
        });

        let config = config_for(&server, BackendKind::LmStudio, None);
        let describer = HttpDescriber::new(config, None);
        let report = describer.probe().expect("probe");

        assert!(report.reachable);
        assert!(report.model_listed);
        assert_eq!(report.vision, Some(false));
        models_mock.assert_calls(1);
        lm_studio_mock.assert_calls(1);
    }

    #[test]
    fn probe_unreachable_is_err_not_panic() {
        let config = DescriberConfig {
            backend: BackendKind::Ollama,
            base_url: "http://127.0.0.1:1".into(),
            model: "test-model".into(),
            api_key: None,
        };
        let describer = HttpDescriber::new(config, None);
        assert!(describer.probe().is_err());
    }
}
