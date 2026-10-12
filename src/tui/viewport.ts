export interface ViewportItemBase {
  id: number;
  role: "user" | "assistant";
  text: string;
  hasLabel: boolean;
  hasUsage: boolean;
}

export interface ViewportWindow<T extends ViewportItemBase> {
  visible: T[];
  hiddenAbove: number;
  hiddenBelow: number;
}

export function wrappedRows(text: string, cols: number): number {
  const width = Math.max(cols - 2, 10);
  let rows = 0;
  for (const line of text.split("\n")) {
    rows += Math.max(1, Math.ceil(line.length / width));
  }
  return rows;
}

export function messageHeight(message: ViewportItemBase, cols: number): number {
  return (
    1 +
    (message.hasLabel ? 1 : 0) +
    wrappedRows(message.text, cols) +
    (message.hasUsage ? 1 : 0)
  );
}

export function windowMessages<T extends ViewportItemBase>(
  messages: ReadonlyArray<T>,
  input: { budgetRows: number; cols: number; scrollOffset: number },
): ViewportWindow<T> {
  const budget = Math.max(input.budgetRows, 0);
  const offset = Math.min(Math.max(input.scrollOffset, 0), Math.max(messages.length - 1, 0));
  if (budget === 0 || messages.length === 0) {
    return { visible: [], hiddenAbove: messages.length - offset, hiddenBelow: offset };
  }
  const startIndex = Math.max(messages.length - offset - 1, 0);
  const visible: T[] = [];
  let used = 0;
  for (let index = startIndex; index >= 0; index--) {
    const message = messages[index];
    if (!message) continue;
    const height = messageHeight(message, input.cols);
    if (used + height > budget && visible.length > 0) break;
    visible.unshift(message);
    used += height;
    if (used >= budget) break;
  }
  const firstVisibleIndex = messages.length - offset - visible.length;
  return {
    visible,
    hiddenAbove: Math.max(firstVisibleIndex, 0),
    hiddenBelow: offset,
  };
}

export function clampOffset(requested: number, messageCount: number): number {
  if (messageCount <= 1) return 0;
  return Math.min(Math.max(requested, 0), messageCount - 1);
}
