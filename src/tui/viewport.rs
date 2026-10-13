#[derive(Debug, Clone)]
pub struct ViewportItem {
    pub id: i64,
    pub role: String,
    pub text: String,
    pub has_label: bool,
    pub has_usage: bool,
}

pub struct ViewportWindow<'a> {
    pub visible: Vec<&'a ViewportItem>,
    pub hidden_above: usize,
    pub hidden_below: usize,
}

pub fn wrapped_rows(text: &str, cols: usize) -> usize {
    let width = cols.saturating_sub(2).max(10);
    let mut rows = 0;
    for line in text.split('\n') {
        rows += 1.max(line.len().div_ceil(width));
    }
    rows
}

pub fn message_height(message: &ViewportItem, cols: usize) -> usize {
    1 + usize::from(message.has_label) + wrapped_rows(&message.text, cols) + usize::from(message.has_usage)
}

pub fn window_messages<'a>(
    messages: &'a [ViewportItem],
    budget_rows: usize,
    cols: usize,
    scroll_offset: usize,
) -> ViewportWindow<'a> {
    if budget_rows == 0 || messages.is_empty() {
        return ViewportWindow {
            visible: Vec::new(),
            hidden_above: messages.len(),
            hidden_below: 0,
        };
    }

    let total_height: usize = messages.iter().map(|message| message_height(message, cols)).sum();

    if scroll_offset == 0 {
        let mut visible: Vec<&ViewportItem> = Vec::new();
        let mut used = 0usize;
        for message in messages.iter().rev() {
            let height = message_height(message, cols);
            if used + height > budget_rows && !visible.is_empty() {
                break;
            }
            visible.push(message);
            used += height;
            if used >= budget_rows {
                break;
            }
        }
        visible.reverse();
        let first_visible_index = messages.len() - visible.len();
        return ViewportWindow {
            visible,
            hidden_above: first_visible_index,
            hidden_below: 0,
        };
    }

    let scroll_from_bottom = scroll_offset.min(total_height.saturating_sub(1));
    let target_from_top = total_height.saturating_sub(budget_rows + scroll_from_bottom);

    let mut visible: Vec<&ViewportItem> = Vec::new();
    let mut cumulative = 0usize;
    let mut first_visible_index = messages.len();

    for (index, message) in messages.iter().enumerate() {
        let height = message_height(message, cols);
        let message_start = cumulative;
        cumulative += height;

        if cumulative <= target_from_top {
            continue;
        }
        if message_start >= target_from_top + budget_rows {
            break;
        }
        if first_visible_index == messages.len() {
            first_visible_index = index;
        }
        visible.push(message);
    }

    let hidden_below = messages.len() - (first_visible_index + visible.len());
    ViewportWindow {
        visible,
        hidden_above: first_visible_index,
        hidden_below,
    }
}
