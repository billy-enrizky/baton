use crate::store::UsageRow;
use crate::types::TurnUsage;

pub fn cache_hit_rate(usage: &TurnUsage) -> Option<f64> {
    let total = usage.cache_read_tokens + usage.cache_creation_tokens + usage.input_tokens;
    if total <= 0 {
        None
    } else {
        Some(usage.cache_read_tokens as f64 / total as f64)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UsageTotals {
    pub turns: usize,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost_usd: Option<f64>,
}

impl UsageTotals {
    pub fn total_usage(&self) -> TurnUsage {
        TurnUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_creation_tokens: self.cache_creation_tokens,
            cost_usd: self.cost_usd,
        }
    }
}

pub fn sum_usage(rows: &[UsageRow]) -> UsageTotals {
    let mut totals = UsageTotals {
        turns: rows.len(),
        ..Default::default()
    };
    let mut saw_cost = false;
    let mut cost_sum = 0.0f64;
    for row in rows {
        totals.input_tokens += row.usage.input_tokens;
        totals.output_tokens += row.usage.output_tokens;
        totals.cache_read_tokens += row.usage.cache_read_tokens;
        totals.cache_creation_tokens += row.usage.cache_creation_tokens;
        if let Some(cost) = row.usage.cost_usd {
            saw_cost = true;
            cost_sum += cost;
        }
    }
    if saw_cost {
        totals.cost_usd = Some((cost_sum * 1e6).round() / 1e6);
    }
    totals
}

pub fn format_token_count(tokens: i64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

pub fn format_usage_line(totals: &UsageTotals) -> String {
    let usage = totals.total_usage();
    let rate = cache_hit_rate(&usage);
    let cache_part = match rate {
        None => "cache n/a".to_string(),
        Some(rate) => format!("cache {:.0}%", rate * 100.0),
    };
    let cost_part = match totals.cost_usd {
        Some(cost) => format!(", ${cost:.2}"),
        None => String::new(),
    };
    let total_input = totals.input_tokens + totals.cache_read_tokens + totals.cache_creation_tokens;
    format!(
        "in {}, out {}, {}{}",
        format_token_count(total_input),
        format_token_count(totals.output_tokens),
        cache_part,
        cost_part
    )
}
