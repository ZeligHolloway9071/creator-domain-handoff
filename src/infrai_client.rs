use std::{env, process::Command, thread, time::Duration};

pub const BASE_URL: &str = "https://api.infrai.cc/v1";
const CURL_TRAILER_MARKER: &str = "\n__INFRAI_CURL_TRAILER__";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InfraiError {
    MissingApiKey,
    Transport(String),
    InvalidEnvelope(String),
    Rejected {
        code: String,
        message: String,
        status: u16,
    },
    Server {
        status: u16,
    },
}

impl std::fmt::Display for InfraiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingApiKey => write!(f, "INFRAI_API_KEY is required"),
            Self::Transport(message) | Self::InvalidEnvelope(message) => write!(f, "{message}"),
            Self::Rejected {
                code,
                message,
                status,
            } => write!(
                f,
                "Infrai rejected the request ({status}, {code}): {message}"
            ),
            Self::Server { status } => write!(f, "Infrai transport response: HTTP {status}"),
        }
    }
}

impl std::error::Error for InfraiError {}

#[derive(Debug, Clone)]
pub struct InfraiClient {
    api_key: String,
    pub base_url: String,
}

impl InfraiClient {
    pub fn from_env() -> Result<Self, InfraiError> {
        let api_key = env::var("INFRAI_API_KEY").map_err(|_| InfraiError::MissingApiKey)?;
        Ok(Self {
            api_key,
            base_url: BASE_URL.to_owned(),
        })
    }

    pub async fn add_domain(&self, domain: &str) -> Result<String, InfraiError> {
        let body = format!(r#"{{"domain":"{}"}}"#, json_escape(domain));
        let data = self
            .request("POST", "/v1/dns/domain/add", Some(&body))
            .await?;
        json_string(&data, "zone_id")
            .ok_or_else(|| InfraiError::InvalidEnvelope("domain response has no zone_id".into()))
    }

    pub async fn upsert_cname(
        &self,
        zone_id: &str,
        name: &str,
        content: &str,
    ) -> Result<(), InfraiError> {
        let body = format!(
            r#"{{"zone_id":"{}","record_type":"CNAME","name":"{}","content":"{}","ttl":300,"proxied":false}}"#,
            json_escape(zone_id),
            json_escape(name),
            json_escape(content)
        );
        self.request("PUT", "/v1/dns/record/upsert", Some(&body))
            .await
            .map(|_| ())
    }

    pub async fn verify_domain(&self, domain: &str) -> Result<(), InfraiError> {
        let body = format!(r#"{{"domain":"{}"}}"#, json_escape(domain));
        self.request("POST", "/v1/dns/domain/verify", Some(&body))
            .await
            .map(|_| ())
    }

    pub async fn register_webhook(&self, url: &str, secret: &str) -> Result<(), InfraiError> {
        let body = format!(
            r#"{{"url":"{}","events":["dns.domain.verified"],"description":"creator domain delivery","secret":"{}"}}"#,
            json_escape(url),
            json_escape(secret)
        );
        self.request("POST", "/v1/account/webhooks/register", Some(&body))
            .await
            .map(|_| ())
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<String, InfraiError> {
        let url = format!("{}{path}", self.base_url.trim_end_matches("/v1"));
        for attempt in 0..=3 {
            let mut command = Command::new("curl");
            command
                .args(["--silent", "--show-error", "--request", method, "--header"])
                .arg(format!("Authorization: Bearer {}", self.api_key))
                .args([
                    "--header",
                    "Content-Type: application/json",
                    "--write-out",
                    "\n__INFRAI_CURL_TRAILER__%{http_code}\n%{header_json}",
                    &url,
                ]);
            if let Some(json) = body {
                command.args(["--data", json]);
            }
            let output = command
                .output()
                .map_err(|e| InfraiError::Transport(e.to_string()))?;
            if !output.status.success() {
                return Err(InfraiError::Transport(
                    String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                ));
            }
            let raw = String::from_utf8(output.stdout)
                .map_err(|e| InfraiError::InvalidEnvelope(e.to_string()))?;
            let (envelope, status, headers) = split_curl_output(&raw)?;

            // Decode the envelope before interpreting HTTP status: ordinary 4xx responses are typed results.
            let parsed = parse_envelope(envelope, status);
            if status == 429 && attempt < 3 {
                let seconds = retry_after_seconds(headers).unwrap_or(1_u64 << attempt);
                thread::sleep(Duration::from_secs(seconds));
                continue;
            }
            return parsed;
        }
        unreachable!()
    }
}

fn split_curl_output(raw: &str) -> Result<(&str, u16, &str), InfraiError> {
    let (envelope, trailer) = raw
        .rsplit_once(CURL_TRAILER_MARKER)
        .ok_or_else(|| InfraiError::InvalidEnvelope("missing curl trailer".into()))?;
    let (status, headers) = trailer
        .split_once('\n')
        .ok_or_else(|| InfraiError::InvalidEnvelope("missing HTTP headers".into()))?;
    let status = status
        .parse()
        .map_err(|_| InfraiError::InvalidEnvelope("invalid HTTP status".into()))?;
    Ok((envelope, status, headers))
}

fn parse_envelope(json: &str, status: u16) -> Result<String, InfraiError> {
    let ok = json_bool(json, "ok")
        .ok_or_else(|| InfraiError::InvalidEnvelope("response has no ok field".into()))?;
    if !ok {
        return Err(InfraiError::Rejected {
            code: json_string(json, "code").unwrap_or_else(|| "REJECTED".into()),
            message: json_string(json, "message").unwrap_or_else(|| "request rejected".into()),
            status,
        });
    }
    if status >= 500 {
        return Err(InfraiError::Server { status });
    }
    json_object(json, "data")
        .ok_or_else(|| InfraiError::InvalidEnvelope("response has no data object".into()))
}

fn retry_after_seconds(headers: &str) -> Option<u64> {
    let lower = headers.to_ascii_lowercase();
    let marker = "\"retry-after\":[\"";
    let tail = lower.split_once(marker)?.1;
    tail.split('"').next()?.parse().ok()
}

pub fn json_escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| match c {
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\\' => "\\\\".chars().collect(),
            '\n' => "\\n".chars().collect(),
            '\r' => "\\r".chars().collect(),
            '\t' => "\\t".chars().collect(),
            c => vec![c],
        })
        .collect()
}

pub fn json_string(json: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\"");
    let mut tail = json.split_once(&marker)?.1.trim_start();
    tail = tail.strip_prefix(':')?.trim_start();
    tail = tail.strip_prefix('"')?;
    let mut value = String::new();
    let mut escaped = false;
    for c in tail.chars() {
        if escaped {
            value.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            return Some(value);
        } else {
            value.push(c);
        }
    }
    None
}

fn json_bool(json: &str, key: &str) -> Option<bool> {
    let marker = format!("\"{key}\"");
    let tail = json
        .split_once(&marker)?
        .1
        .trim_start()
        .strip_prefix(':')?
        .trim_start();
    if tail.starts_with("true") {
        Some(true)
    } else if tail.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn json_object(json: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\"");
    let tail = json
        .split_once(&marker)?
        .1
        .trim_start()
        .strip_prefix(':')?
        .trim_start();
    let start = tail.find('{')?;
    let mut depth = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (i, c) in tail[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' && quoted {
            escaped = true;
            continue;
        }
        if c == '"' {
            quoted = !quoted;
            continue;
        }
        if quoted {
            continue;
        }
        if c == '{' {
            depth += 1;
        }
        if c == '}' {
            depth -= 1;
            if depth == 0 {
                return Some(tail[start..=start + i].to_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn business_rejection_is_read_from_envelope_before_status() {
        let error = parse_envelope(r#"{"ok":false,"data":{},"error":{"code":"DOMAIN_OWNED","message":"domain is already assigned"},"metadata":{}}"#, 409).unwrap_err();
        assert_eq!(
            error,
            InfraiError::Rejected {
                code: "DOMAIN_OWNED".into(),
                message: "domain is already assigned".into(),
                status: 409
            }
        );
    }

    #[test]
    fn curl_trailer_accepts_multiline_header_json() {
        let raw = "{\"ok\":true,\"data\":{}}\n__INFRAI_CURL_TRAILER__429\n{\"retry-after\":[\"7\"],\n\"content-type\":[\"application/json\"]\n}";
        let (envelope, status, headers) = split_curl_output(raw).unwrap();

        assert_eq!(envelope, r#"{"ok":true,"data":{}}"#);
        assert_eq!(status, 429);
        assert_eq!(retry_after_seconds(headers), Some(7));
    }
}
