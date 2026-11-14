use crate::{
    app::{destinations::DestinationId, error::AppError},
    persistence::settings::{LinkOrigin, ServerOrigin},
};
use serde::Deserialize;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploaderInfoSupport {
    Available(ValidatedUploaderInfo),
    Unsupported404,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainChoice {
    Https(LinkOrigin),
    UnsupportedHttp(String),
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectiveSettings {
    pub expires: String,
    #[serde(deserialize_with = "required_option")]
    pub max_views: Option<u32>,
    pub name_format: NameFormat,
    pub convert: Conversion,
    pub keep_name: bool,
    pub password_protected: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NameFormat {
    Random,
    Date,
    Filename,
    Gfycat,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Conversion {
    Off,
    Png,
    Jpg,
    Webp,
}
fn required_option<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    Option::<u32>::deserialize(d)
}
#[derive(Clone, PartialEq, Eq)]
pub struct ValidatedUploaderInfo {
    pub name: String,
    pub domains: Vec<DomainChoice>,
    pub domain: DomainChoice,
    pub settings: EffectiveSettings,
}
impl std::fmt::Debug for ValidatedUploaderInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidatedUploaderInfo")
            .field("name", &"<redacted>")
            .field("domain_count", &self.domains.len())
            .field("settings", &self.settings)
            .finish()
    }
}
impl ValidatedUploaderInfo {
    pub(crate) fn parse(body: &[u8]) -> Result<Self, AppError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Dto {
            name: String,
            domains: Vec<String>,
            domain: String,
            settings: EffectiveSettings,
        }
        let d: Dto = serde_json::from_slice(body)
            .map_err(|_| AppError::Protocol("malformed uploader metadata"))?;
        if d.name.trim().is_empty()
            || d.name.chars().count() > 100
            || d.name.chars().any(char::is_control)
            || d.domains.len() > 256
            || d.settings
                .max_views
                .is_some_and(|v| v == 0 || v > i32::MAX as u32)
            || !valid_expiry(&d.settings.expires)
        {
            return Err(AppError::Protocol("invalid uploader metadata"));
        }
        Ok(Self {
            name: d.name,
            domains: d
                .domains
                .iter()
                .map(|s| domain(s))
                .collect::<Result<_, _>>()?,
            domain: domain(&d.domain)?,
            settings: d.settings,
        })
    }
}
fn domain(s: &str) -> Result<DomainChoice, AppError> {
    if let Ok(origin) = LinkOrigin::parse(s) {
        return Ok(DomainChoice::Https(origin));
    }
    // HTTP entries are display-only; reuse strict parser after scheme replacement.
    if let Some(rest) = s.strip_prefix("http://") {
        LinkOrigin::parse(&format!("https://{rest}"))
            .map_err(|_| AppError::Protocol("invalid metadata domain"))?;
        return Ok(DomainChoice::UnsupportedHttp(
            url::Url::parse(s)
                .map_err(|_| AppError::Protocol("invalid metadata domain"))?
                .origin()
                .ascii_serialization(),
        ));
    }
    Err(AppError::Protocol("invalid metadata domain"))
}
// Go duration grammar with checked integer nanoseconds, including Vectr's
// decimal-day extension. Bound text and signed i64 duration; no float overflow.
fn valid_expiry(s: &str) -> bool {
    if s == "never" {
        return true;
    }
    if s.is_empty() || s.len() > 128 {
        return false;
    }
    let mut text = s.strip_prefix('+').unwrap_or(s);
    let days = text.ends_with('d');
    if days {
        let number = &s[..s.len() - 1];
        let mut parts = number.split('.');
        let whole = parts.next().unwrap_or_default();
        let fraction = parts.next();
        if whole.is_empty()
            || !whole.bytes().all(|c| c.is_ascii_digit())
            || parts.next().is_some()
            || fraction.is_some_and(|f| f.is_empty() || !f.bytes().all(|c| c.is_ascii_digit()))
        {
            return false;
        }
    }
    let mut total = 0u128;
    while !text.is_empty() {
        let end = text
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(text.len());
        if end == 0 {
            return false;
        }
        let number = &text[..end];
        text = &text[end..];
        let units = [
            ("ns", 1u128),
            ("us", 1000),
            ("µs", 1000),
            ("μs", 1000),
            ("ms", 1_000_000),
            ("s", 1_000_000_000),
            ("m", 60_000_000_000),
            ("h", 3_600_000_000_000),
            ("d", 86_400_000_000_000),
        ];
        let Some((unit, mult)) = units.iter().find(|(u, _)| text.starts_with(u)) else {
            return false;
        };
        if *unit == "d" && !days {
            return false;
        }
        if days && (*unit != "d" || !text[unit.len()..].is_empty()) {
            return false;
        }
        let mut parts = number.split('.');
        let whole = parts.next().unwrap();
        let frac = parts.next();
        if parts.next().is_some() || (whole.is_empty() && frac.is_none_or(str::is_empty)) {
            return false;
        }
        let Ok(whole) = (if whole.is_empty() {
            Ok(0u128)
        } else {
            whole.parse::<u128>()
        }) else {
            return false;
        };
        let Some(mut n) = whole.checked_mul(*mult) else {
            return false;
        };
        if n > i64::MAX as u128 {
            return false;
        }
        if let Some(frac) = frac {
            // Reverse decimal multiplication preserves sub-nanosecond truncation
            // without overflowing on long, otherwise valid fractional strings.
            let fractional = frac.bytes().rev().fold(0u128, |carry, digit| {
                (u128::from(digit - b'0') * mult + carry) / 10
            });
            let Some(sum) = n.checked_add(fractional) else {
                return false;
            };
            n = sum;
        }
        let Some(sum) = total.checked_add(n) else {
            return false;
        };
        total = sum;
        if total > i64::MAX as u128 {
            return false;
        }
        text = &text[unit.len()..];
    }
    total > 0
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataKey {
    pub destination: DestinationId,
    pub origin: ServerOrigin,
    pub revision: u64,
}
/// Consumer keeps this key with its request; accept only if selection and
/// credential/settings revision still match when the asynchronous result arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrelatedMetadata {
    pub key: MetadataKey,
    pub info: UploaderInfoSupport,
}
impl CorrelatedMetadata {
    pub fn into_current(self, current: &MetadataKey) -> Option<UploaderInfoSupport> {
        if &self.key == current {
            Some(self.info)
        } else {
            None
        }
    }
}
