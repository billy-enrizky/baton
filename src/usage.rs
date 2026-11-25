use crate::store::UsageRow;
use crate::types::TurnUsage;
use std::collections::BTreeMap;

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
    let mut sum = UsageSum::default();
    for row in rows {
        sum.add(row);
    }
    sum.finish()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageGroup {
    Day,
    Model,
    Provider,
}

impl UsageGroup {
    pub fn as_str(self) -> &'static str {
        match self {
            UsageGroup::Day => "day",
            UsageGroup::Model => "model",
            UsageGroup::Provider => "provider",
        }
    }
}

/// Totals per key. Days are newest first; model and provider keys are
/// alphabetical so two runs of the same history print the same order.
pub fn group_usage(rows: &[UsageRow], group: UsageGroup) -> Vec<(String, UsageTotals)> {
    let mut groups: BTreeMap<String, UsageSum> = BTreeMap::new();
    for row in rows {
        let key = match group {
            UsageGroup::Day => row.created_at.chars().take(10).collect::<String>(),
            UsageGroup::Model => row
                .model
                .as_deref()
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .unwrap_or("unknown")
                .to_string(),
            UsageGroup::Provider => row.provider.as_str().to_string(),
        };
        groups.entry(key).or_default().add(row);
    }
    let mut grouped: Vec<(String, UsageTotals)> = groups
        .into_iter()
        .map(|(key, sum)| (key, sum.finish()))
        .collect();
    if group == UsageGroup::Day {
        grouped.reverse();
    }
    grouped
}

#[derive(Debug, Default)]
struct UsageSum {
    totals: UsageTotals,
    cost_sum: f64,
    saw_cost: bool,
}

impl UsageSum {
    fn add(&mut self, row: &UsageRow) {
        self.totals.turns += 1;
        self.totals.input_tokens += row.usage.input_tokens;
        self.totals.output_tokens += row.usage.output_tokens;
        self.totals.cache_read_tokens += row.usage.cache_read_tokens;
        self.totals.cache_creation_tokens += row.usage.cache_creation_tokens;
        if let Some(cost) = row.usage.cost_usd {
            self.saw_cost = true;
            self.cost_sum += cost;
        }
    }

    fn finish(mut self) -> UsageTotals {
        if self.saw_cost {
            self.totals.cost_usd = Some((self.cost_sum * 1e6).round() / 1e6);
        }
        self.totals
    }
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
