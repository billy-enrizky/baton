# Approval choices

## Sub-features
- Every approval opens one overlay with the provider's own choices when the protocol supplies them, and the three standard choices otherwise.
- ACP `session/request_permission` options become rows with their `name`, `optionId`, and `kind`.
- Up, Down, j, k, PageUp, PageDown, Home, and End move the selected row. Enter sends the selected choice.
- The y, a, and n shortcuts still send allow once, allow for session, and deny.
- A child-thread approval names the first eight characters of the child thread id and stays on the same stack as an open picker or review.
- The exec JSON stream includes the choices and never includes the responder.

## How to get to it (user POV)

Run a turn that asks for permission. The overlay appears above the composer with the provider's choices. Select a row with the arrow keys and press Enter, or use y, a, or n. A request from a child thread keeps the child prefix in the header.

## Approach

`TurnEvent::ApprovalRequest` carries `choices`. `ApprovalState` stores them and the selected index. `decision_for_choice` maps the standard ids `accept`, `accept_always`, and `deny` back to the existing `ApprovalDecision` variants, and sends `ApprovalDecision::Choose(option_id)` for a provider-specific id. ACP builds the rows from `optionId`, `name`, and `kind` in `permission_choices`, and `pick_permission_option` accepts a `Choose` id only when it is one of the offered options; an unknown id becomes a cancelled permission response. The other adapters publish the standard choices and deny a `Choose` they do not understand.

## Gotchas
- ACP accepts only an option id, so a free-text custom answer is not part of this protocol.
- The overlay shows eight rows at a time and keeps the selected row in the window.
- A child thread id is shortened to eight characters in the header.
- Enter on an empty choice list falls back to Deny. The adapters always provide at least the standard three.

## Proof

- `approval_picker_selects_a_provider_choice_with_arrows_and_enter`
- `child_approval_overlay_shows_every_choice_and_the_child_thread`
- `approvals_stack_over_pickers_without_losing_them`
- `acp_adapter_maps_permission_decisions_to_options`
- `permission_choices_keep_the_provider_labels_and_ids`
- `a_typed_choice_must_be_one_of_the_offered_options`
- `approval_events_do_not_leak_the_responder`
- `approval_probe.py` runs a fake `agy` ACP agent, selects `opt-reject` with two Down presses and Enter, and checks the response the agent received.
