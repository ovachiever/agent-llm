use crate::types::ProviderKind;

#[derive(Debug, Clone, Copy)]
pub struct PricingRule {
    pub prompt_per_million: f64,
    pub completion_per_million: f64,
}

pub fn lookup(provider: ProviderKind, model: &str) -> Option<PricingRule> {
    let lower = model.to_ascii_lowercase();

    let rule = match provider {
        ProviderKind::OpenAi if lower.contains("gpt-4.1") => PricingRule {
            prompt_per_million: 2.0,
            completion_per_million: 8.0,
        },
        ProviderKind::OpenAi if lower.contains("gpt-4o") => PricingRule {
            prompt_per_million: 5.0,
            completion_per_million: 15.0,
        },
        ProviderKind::Anthropic if lower.contains("opus") => PricingRule {
            prompt_per_million: 15.0,
            completion_per_million: 75.0,
        },
        ProviderKind::Anthropic if lower.contains("sonnet") => PricingRule {
            prompt_per_million: 3.0,
            completion_per_million: 15.0,
        },
        ProviderKind::Google if lower.contains("gemini-2.5-pro") => PricingRule {
            prompt_per_million: 3.5,
            completion_per_million: 10.5,
        },
        ProviderKind::Google if lower.contains("gemini") => PricingRule {
            prompt_per_million: 1.25,
            completion_per_million: 5.0,
        },
        ProviderKind::OpenRouter if lower.contains("anthropic/claude-opus") => PricingRule {
            prompt_per_million: 15.0,
            completion_per_million: 75.0,
        },
        ProviderKind::OpenRouter if lower.contains("openai/gpt-4.1") => PricingRule {
            prompt_per_million: 2.0,
            completion_per_million: 8.0,
        },
        _ => return None,
    };

    Some(rule)
}

pub fn estimate_cost_usd(
    provider: ProviderKind,
    model: &str,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
) -> Option<f64> {
    let pricing = lookup(provider, model)?;
    let prompt_cost =
        prompt_tokens.unwrap_or_default() as f64 / 1_000_000.0 * pricing.prompt_per_million;
    let completion_cost =
        completion_tokens.unwrap_or_default() as f64 / 1_000_000.0 * pricing.completion_per_million;
    Some(prompt_cost + completion_cost)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_openai_costs() {
        let cost = estimate_cost_usd(
            ProviderKind::OpenAi,
            "gpt-4.1",
            Some(1_000_000),
            Some(500_000),
        )
        .expect("expected pricing rule");

        assert!((cost - 6.0).abs() < 0.0001);
    }

    #[test]
    fn returns_none_for_unknown_model() {
        assert!(
            estimate_cost_usd(
                ProviderKind::OpenAi,
                "totally-unknown",
                Some(100),
                Some(100)
            )
            .is_none()
        );
    }
}
