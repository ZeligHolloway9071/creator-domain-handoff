use crate::infrai_client::{json_escape, json_string, InfraiClient, InfraiError};
use crate::webhook_signature;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryState {
    Processing,
    AwaitingDomain,
    ReadyForCreatorDelivery,
}

#[derive(Debug, Clone)]
pub struct MediaOnboarding {
    pub asset_id: String,
    pub processing_job_id: String,
    pub domain: String,
    pub creator_origin: String,
    pub state: DeliveryState,
}

#[derive(Debug)]
pub enum ServiceError {
    Infrai(InfraiError),
    InvalidInput(&'static str),
    InvalidSignature,
    InvalidEvent,
}

impl From<InfraiError> for ServiceError {
    fn from(value: InfraiError) -> Self {
        Self::Infrai(value)
    }
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Infrai(error) => write!(f, "{error}"),
            Self::InvalidInput(message) => write!(f, "{message}"),
            Self::InvalidSignature => write!(f, "invalid webhook signature"),
            Self::InvalidEvent => write!(f, "invalid webhook event"),
        }
    }
}

impl std::error::Error for ServiceError {}

pub async fn onboard(
    client: &InfraiClient,
    mut media: MediaOnboarding,
    webhook_url: &str,
    webhook_secret: &str,
) -> Result<MediaOnboarding, ServiceError> {
    if media.state != DeliveryState::Processing {
        return Err(ServiceError::InvalidInput("asset must be processing"));
    }
    let zone_id = client.add_domain(&media.domain).await?;
    client
        .upsert_cname(&zone_id, &media.domain, &media.creator_origin)
        .await?;
    client.register_webhook(webhook_url, webhook_secret).await?;
    client.verify_domain(&media.domain).await?;
    media.state = DeliveryState::AwaitingDomain;
    Ok(media)
}

pub fn accept_verified_domain(
    media: &mut MediaOnboarding,
    payload: &[u8],
    signature: &str,
    webhook_secret: &str,
) -> Result<String, ServiceError> {
    if !webhook_signature::verify(webhook_secret.as_bytes(), payload, signature) {
        return Err(ServiceError::InvalidSignature);
    }
    let text = std::str::from_utf8(payload).map_err(|_| ServiceError::InvalidEvent)?;
    let domain = json_string(text, "domain").ok_or(ServiceError::InvalidEvent)?;
    if domain != media.domain || media.state != DeliveryState::AwaitingDomain {
        return Err(ServiceError::InvalidEvent);
    }
    media.state = DeliveryState::ReadyForCreatorDelivery;
    Ok(format!(
        r#"{{"asset_id":"{}","processing_job_id":"{}","domain":"{}","state":"ready_for_creator_delivery"}}"#,
        json_escape(&media.asset_id),
        json_escape(&media.processing_job_id),
        json_escape(&media.domain)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_domain_releases_the_processed_asset() {
        let mut media = MediaOnboarding {
            asset_id: "asset-42".into(),
            processing_job_id: "job-9".into(),
            domain: "watch.creator.test".into(),
            creator_origin: "delivery.media.test".into(),
            state: DeliveryState::AwaitingDomain,
        };
        let payload = br#"{"domain":"watch.creator.test"}"#;
        let signature = "790b07c0fde520081dd2c5c588902ab34518321e9d800d40d78715078f5d7362";
        let result =
            accept_verified_domain(&mut media, payload, signature, "signing-secret").unwrap();
        assert_eq!(media.state, DeliveryState::ReadyForCreatorDelivery);
        assert!(result.contains("ready_for_creator_delivery"));
    }
}
