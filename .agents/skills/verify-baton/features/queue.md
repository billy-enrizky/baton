# Queued follow-ups

## Sub-features
- A message sent while a turn is running is steered on a harness that supports it and queued durably otherwise.
- `/queue` opens a picker over the current thread's queued messages.
- Enter sends the selected item now, `e` hands an `/queue edit <n> ` command to the composer, `d` deletes the selected item, and `c` twice clears the queue.
- Ctrl+Up and Ctrl+Down reorder the selected item and persist the new order.
- `/queue send <n>`, `/queue edit <n> <text>`, `/queue move <from> <to>`, and `/queue clear` remain for scripts and exact commands.

## How to get to it (user POV)

While a turn runs, send a second message. On Codex, Claude, and OpenCode it is steered into the live turn; elsewhere it is queued. Type `/queue` to see the queue. The picker header names the count and every key, and each row is a one-line preview.

## Approach

`TuiState.queued_messages` mirrors the durable `queued_messages` table. The picker reads that list and writes through `replace_queued_messages`, `take_queued_message`, and `clear_queued_messages`, so a delete, move, send, or clear survives a restart. Sending removes the row before dispatching, which keeps a failed dispatch from duplicating the message. The picker closes when the last item is deleted.

## Alternatives considered

A second action picker per item would add a level for four actions that fit in the footer. A free-form editor inside the overlay would duplicate the composer. The picker hands edit back to the composer instead.

## Gotchas
- `c` clears only after a second press. The first press turns the header into a warning that names the count.
- The overlay shows at most eight rows. Use the exact `/queue move` command for a queue longer than eight when the target is off screen.
- A queued item is per thread. Switching threads shows that thread's queue.
- Sending a queued item is not undoable from the TUI; the transcript records the sent text.

## Proof

- `queue_command_opens_a_picker_and_sends_the_selected_item`
- `queue_picker_deletes_and_clears_with_a_second_press`
- `queue_picker_moves_the_selected_item_and_hands_edit_to_the_composer`
- `queue_command_lists_and_clears_persisted_messages`
- `queue_send_edit_and_move_control_the_durable_queue`
- `queue_rejects_out_of_range_positions_and_keeps_the_queue`
- `a_successful_turn_pops_the_persisted_queue`
