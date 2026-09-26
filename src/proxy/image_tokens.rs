use crate::config::{AppConfig, Provider, ProviderFamily};
use base64::{
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
    Engine as _,
};
use serde_json::Value;

pub(super) const CHARS_PER_TOKEN: usize = 3;
pub(super) const DEFAULT_IMAGE_TOKENS: usize = 1_024;

const DEEPSEEK_FLASH_IMAGE_TOKENS: usize = 256;
const OPENAI_LOW_DETAIL_TOKENS: usize = 85;
const OPENAI_TILE_TOKENS: usize = 170;
const OPENAI_TILE_SIZE: u64 = 512;
const OPENAI_MAX_DIMENSION: u64 = 2_048;
const OPENAI_MIN_SIDE: u64 = 768;
const ANTHROPIC_MAX_DIMENSION: u64 = 1_568;
const ANTHROPIC_PIXELS_PER_TOKEN: u64 = 750;
const ANTHROPIC_MAX_TOKENS: usize = 4_096;

/// Pre-request image accounting for a routed model. Provider APIs describe
/// image cost differently, so the proxy needs a policy rather than treating
/// every content part as text. Unknown models get a bounded conservative
/// value, never the encoded data-URL length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ImageTokenPolicy {
    Fixed(usize),
    OpenAiHighDetail,
    Anthropic,
}

impl ImageTokenPolicy {
    pub(super) fn fallback_tokens(self) -> usize {
        match self {
            Self::Fixed(tokens) => tokens,
            Self::OpenAiHighDetail => 85 + 170 * 4,
            Self::Anthropic => DEFAULT_IMAGE_TOKENS,
        }
    }

    pub(super) fn estimate(self, url: &str, detail: Option<&str>) -> usize {
        let Some((width, height)) = image_dimensions_from_data_url(url) else {
            return self.fallback_tokens();
        };
        match self {
            Self::Fixed(tokens) => tokens,
            Self::OpenAiHighDetail if detail == Some("low") => OPENAI_LOW_DETAIL_TOKENS,
            Self::OpenAiHighDetail => openai_high_detail_tokens(width, height),
            Self::Anthropic => anthropic_tokens(width, height),
        }
    }
}

/// Resolve the image accounting policy for one exact routed destination.
/// An explicit `provider/model` override wins, then the provider family and
/// model id select a known formula, and everything else uses the bounded
/// fallback.
pub(super) fn policy_for_model(
    config: &AppConfig,
    provider: &Provider,
    upstream_model: &str,
) -> ImageTokenPolicy {
    let slug = format!("{}/{}", provider.id, upstream_model);
    if let Some(tokens) = config.image_token_overrides.get(&slug) {
        return ImageTokenPolicy::Fixed(*tokens);
    }

    match crate::providers::family_for(provider) {
        ProviderFamily::DeepSeek => {
            if upstream_model.contains("flash") || upstream_model.contains("vision") {
                ImageTokenPolicy::Fixed(DEEPSEEK_FLASH_IMAGE_TOKENS)
            } else {
                ImageTokenPolicy::Fixed(DEFAULT_IMAGE_TOKENS)
            }
        }
        ProviderFamily::Anthropic => ImageTokenPolicy::Anthropic,
        ProviderFamily::OpenRouter => openrouter_policy(upstream_model),
        ProviderFamily::Kimi => ImageTokenPolicy::Fixed(DEFAULT_IMAGE_TOKENS),
        ProviderFamily::OpenAi => {
            if is_openai_vision_model(upstream_model) {
                ImageTokenPolicy::OpenAiHighDetail
            } else {
                ImageTokenPolicy::Fixed(DEFAULT_IMAGE_TOKENS)
            }
        }
    }
}

fn openrouter_policy(model: &str) -> ImageTokenPolicy {
    let model = model.to_ascii_lowercase();
    if model.contains("claude") || model.contains("anthropic") {
        ImageTokenPolicy::Anthropic
    } else if is_openai_vision_model(&model) {
        ImageTokenPolicy::OpenAiHighDetail
    } else if model.contains("deepseek") {
        ImageTokenPolicy::Fixed(DEEPSEEK_FLASH_IMAGE_TOKENS)
    } else {
        ImageTokenPolicy::Fixed(DEFAULT_IMAGE_TOKENS)
    }
}

fn is_openai_vision_model(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.starts_with("gpt-")
        || model.contains("/gpt-")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
}

fn openai_high_detail_tokens(width: u32, height: u32) -> usize {
    let (width, height) = scale_to_fit(width, height, OPENAI_MAX_DIMENSION, OPENAI_MIN_SIDE);
    let tiles = width.div_ceil(OPENAI_TILE_SIZE) * height.div_ceil(OPENAI_TILE_SIZE);
    OPENAI_LOW_DETAIL_TOKENS + OPENAI_TILE_TOKENS * tiles as usize
}

fn anthropic_tokens(width: u32, height: u32) -> usize {
    let (width, height) = scale_to_fit(width, height, ANTHROPIC_MAX_DIMENSION, 0);
    let pixels = width * height;
    let tokens = pixels.div_ceil(ANTHROPIC_PIXELS_PER_TOKEN);
    usize::try_from(tokens)
        .unwrap_or(ANTHROPIC_MAX_TOKENS)
        .clamp(1, ANTHROPIC_MAX_TOKENS)
}

fn scale_to_fit(width: u32, height: u32, max_dimension: u64, min_side: u64) -> (u64, u64) {
    let width = u64::from(width.max(1));
    let height = u64::from(height.max(1));
    let mut scale = 1.0_f64.min(max_dimension as f64 / width.max(height) as f64);
    if min_side > 0 {
        scale = scale.min(min_side as f64 / width.min(height) as f64);
    }
    if scale >= 1.0 {
        return (width, height);
    }
    (
        ((width as f64 * scale).floor() as u64).max(1),
        ((height as f64 * scale).floor() as u64).max(1),
    )
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct TokenEstimate {
    text_chars: usize,
    image_count: usize,
    image_tokens: usize,
}

impl TokenEstimate {
    fn add(self, other: Self) -> Self {
        Self {
            text_chars: self.text_chars.saturating_add(other.text_chars),
            image_count: self.image_count.saturating_add(other.image_count),
            image_tokens: self.image_tokens.saturating_add(other.image_tokens),
        }
    }

    fn tokens(self) -> usize {
        self.text_chars / CHARS_PER_TOKEN + self.image_tokens
    }
}

pub(super) fn estimate_tokens(items: &[Value], policy: ImageTokenPolicy) -> usize {
    items
        .iter()
        .map(|item| measure_value(item, policy))
        .fold(TokenEstimate::default(), TokenEstimate::add)
        .tokens()
}

pub(super) fn estimate_non_input_tokens(
    payload: &Value,
    items: &[Value],
    policy: ImageTokenPolicy,
) -> usize {
    measure_value(payload, policy)
        .tokens()
        .saturating_sub(estimate_tokens(items, policy))
}

fn measure_value(value: &Value, policy: ImageTokenPolicy) -> TokenEstimate {
    if let Some((url, detail)) = image_part(value) {
        let mut estimate = TokenEstimate {
            image_count: 1,
            image_tokens: policy.estimate(url, detail),
            ..TokenEstimate::default()
        };
        estimate.text_chars = estimate.text_chars.saturating_add(2);
        for (key, child) in value.as_object().into_iter().flatten() {
            if key == "image_url" {
                continue;
            }
            estimate.text_chars = estimate
                .text_chars
                .saturating_add(serialized_string_len(key))
                .saturating_add(1);
            estimate = estimate.add(measure_value(child, policy));
        }
        return estimate;
    }

    match value {
        Value::Null => TokenEstimate {
            text_chars: 4,
            ..TokenEstimate::default()
        },
        Value::Bool(value) => TokenEstimate {
            text_chars: if *value { 4 } else { 5 },
            ..TokenEstimate::default()
        },
        Value::Number(value) => TokenEstimate {
            text_chars: value.to_string().len(),
            ..TokenEstimate::default()
        },
        Value::String(value) => TokenEstimate {
            text_chars: serialized_string_len(value),
            ..TokenEstimate::default()
        },
        Value::Array(items) => {
            let mut estimate = TokenEstimate {
                text_chars: 2 + items.len().saturating_sub(1),
                ..TokenEstimate::default()
            };
            for item in items {
                estimate = estimate.add(measure_value(item, policy));
            }
            estimate
        }
        Value::Object(object) => {
            let mut estimate = TokenEstimate {
                text_chars: 2 + object.len().saturating_sub(1),
                ..TokenEstimate::default()
            };
            for (key, child) in object {
                estimate.text_chars = estimate
                    .text_chars
                    .saturating_add(serialized_string_len(key))
                    .saturating_add(1);
                estimate = estimate.add(measure_value(child, policy));
            }
            estimate
        }
    }
}

fn image_part(value: &Value) -> Option<(&str, Option<&str>)> {
    let object = value.as_object()?;
    match object.get("type").and_then(Value::as_str) {
        Some("input_image") => {
            let url = object.get("image_url").and_then(Value::as_str)?;
            let detail = object.get("detail").and_then(Value::as_str);
            Some((url, detail))
        }
        Some("image_url") => {
            let image = object.get("image_url")?.as_object()?;
            let url = image.get("url").and_then(Value::as_str)?;
            let detail = image.get("detail").and_then(Value::as_str);
            Some((url, detail))
        }
        _ => None,
    }
}

fn serialized_string_len(value: &str) -> usize {
    serde_json::to_string(value)
        .map(|serialized| serialized.len())
        .unwrap_or(value.len())
}

fn image_dimensions_from_data_url(url: &str) -> Option<(u32, u32)> {
    let (metadata, encoded) = url.split_once(',')?;
    if !metadata.starts_with("data:image/") || !metadata.ends_with(";base64") {
        return None;
    }
    let prefix_len = encoded.len().min(256 * 1024);
    let prefix_len = prefix_len - prefix_len % 4;
    if prefix_len == 0 {
        return None;
    }
    let encoded_prefix = &encoded[..prefix_len];
    let bytes = STANDARD
        .decode(encoded_prefix)
        .or_else(|_| STANDARD_NO_PAD.decode(encoded_prefix))
        .ok()?;
    png_dimensions(&bytes).or_else(|| jpeg_dimensions(&bytes))
}

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || &bytes[..8] != PNG_SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        return None;
    }
    let mut cursor = 2;
    while cursor + 4 <= bytes.len() {
        if bytes[cursor] != 0xff {
            cursor += 1;
            continue;
        }
        while cursor < bytes.len() && bytes[cursor] == 0xff {
            cursor += 1;
        }
        let marker = *bytes.get(cursor)?;
        cursor += 1;
        if marker == 0xd9 || marker == 0xda {
            return None;
        }
        if matches!(marker, 0x01 | 0xd0..=0xd8) {
            continue;
        }
        let segment_len = usize::from(u16::from_be_bytes([
            *bytes.get(cursor)?,
            *bytes.get(cursor + 1)?,
        ]));
        if segment_len < 2 || cursor + segment_len > bytes.len() {
            return None;
        }
        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) {
            let height = u16::from_be_bytes([*bytes.get(cursor + 3)?, *bytes.get(cursor + 4)?]);
            let width = u16::from_be_bytes([*bytes.get(cursor + 5)?, *bytes.get(cursor + 6)?]);
            return Some((u32::from(width), u32::from(height)));
        }
        cursor += segment_len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_data_url(width: u32, height: u32, payload_bytes: usize) -> String {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&13_u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        png.resize(payload_bytes, 0);
        format!("data:image/png;base64,{}", STANDARD.encode(png))
    }

    #[test]
    fn png_dimensions_are_read_from_the_data_url_header() {
        let url = png_data_url(1_323, 1_871, 32);
        assert_eq!(image_dimensions_from_data_url(&url), Some((1_323, 1_871)));
    }

    #[test]
    fn base64_image_length_does_not_change_the_estimate() {
        let small = png_data_url(1_323, 1_871, 32);
        let large = png_data_url(1_323, 1_871, 4 * 1024 * 1024);
        let policy = ImageTokenPolicy::Fixed(DEEPSEEK_FLASH_IMAGE_TOKENS);
        let small_item = serde_json::json!({
            "type": "function_call_output",
            "output": [{"type": "input_image", "image_url": small, "detail": "high"}]
        });
        let large_item = serde_json::json!({
            "type": "function_call_output",
            "output": [{"type": "input_image", "image_url": large, "detail": "high"}]
        });
        assert_eq!(
            estimate_tokens(&[small_item], policy),
            estimate_tokens(&[large_item], policy)
        );
    }

    #[test]
    fn openai_high_detail_uses_tiles_after_resizing() {
        assert_eq!(openai_high_detail_tokens(1_024, 1_024), 765);
        assert_eq!(openai_high_detail_tokens(513, 513), 765);
    }

    #[test]
    fn anthropic_uses_scaled_pixel_area() {
        assert_eq!(anthropic_tokens(750, 1), 1);
        assert_eq!(anthropic_tokens(1_500, 750), 1_500);
    }
}
