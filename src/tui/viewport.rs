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
    let offset = scroll_offset.min(messages.len().saturating_sub(1));
    if budget_rows == 0 || messages.is_empty() {
        return ViewportWindow {
            visible: Vec::new(),
            hidden_above: messages.len() - offset,
            hidden_below: offset,
        };
    }
    let start_index = messages.len().saturating_sub(offset + 1);
    let mut visible: Vec<&ViewportItem> = Vec::new();
    let mut used = 0usize;
    for message in messages[..=start_index].iter().rev() {
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
    let first_visible_index = messages.len() - offset - visible.len();
    ViewportWindow {
        hidden_above: first_visible_index,
        hidden_below: offset,
        visible,
    }
}
