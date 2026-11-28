# Provider questions

## Sub-features
- Codex `item/tool/requestUserInput` becomes a question overlay instead of an unhandled server request.
- Claude Code's `AskUserQuestion` tool arrives as a `can_use_tool` control request. Its `questions` array becomes the same overlay, and the answer is returned as an `allow` result carrying `updatedInput.answers` keyed by question text.
- OpenCode 2 sends a `form.created` event mid-turn. Its fields become the same overlay, the reply posts to `/api/session/{session}/form/{form}/reply` with an `answer` map, and a dismissed or unanswerable form is cancelled on the same route.
- Grok ACP `x.ai/ask_user_question` and `_x.ai/ask_user_question` requests use the same overlay. A question without options gets an `OK` row, and answers are keyed by the question text in the xAI response.
- A provider question with `multiSelect` renders checkboxes. Space toggles the highlighted option, Enter submits every checked label in option order, and an empty selection does not submit.
- Antigravity sends user input as `session/request_permission` with a `toolCallId` that starts with `interaction_`. Those requests use the question overlay, show the provider option labels, and return the selected `optionId`; ordinary permission requests still use the approval overlay.
- One request can carry multiple questions. The TUI walks them one at a time and sends one batched answers map.
- Each question lists its options. The selected option is sent as the answer.
- A question with `isOther` shows an `Other (type an answer)` row. Enter opens the custom input, Enter submits it, and Escape returns to the options.
- A question with `isSecret` masks the custom input with `*` while keeping the typed answer.
- A child-thread question names the first eight characters of the child thread id.
- The exec JSON stream includes the questions and never includes the responder.

## How to get to it (user POV)

Run a Codex turn that asks a clarifying question, for example from Plan mode or a prompt that asks Codex to request one. The question overlay appears above the composer. Use Up and Down to choose an option and Enter to answer. Choose Other when a custom answer is allowed, type the answer, and press Enter. Multiple questions are shown in order; the turn continues after the last answer. Grok and Antigravity use the same overlay when their ACP agents ask for input. For a multi-select question, press Space on each option and Enter once.

## Approach

`TurnEvent::QuestionRequest` carries `QuestionPrompt` values and a responder for `Vec<QuestionAnswer>`. Claude `question_control_response` parses `questions`, `options`, and `multiSelect`, emits the overlay, and answers `question_response`, which keys each answer by the question text and joins a multi-select answer into one comma-separated string; a dismissed question denies the tool because an `allow` with no answers would hand the model an empty result. Codex `handle_user_input_request` parses `questions`, `options`, `isOther`, `isSecret`, and an optional `multiSelect`, then `question_response` builds the native `{answers:{id:{answers:[...]}}}` result. ACP uses `handle_question_request` for both Grok and Antigravity: Grok builds the xAI text-keyed response, while Antigravity maps the answered label back to exactly one provider `optionId` and refuses an ambiguous or unknown label. The TUI stores a `QuestionState` per request, walks `index`, and `finish_question_answers` either advances to the next question or sends the batch. `question_key` handles the option cursor, Space toggles, and the custom input; `question_selected_labels` preserves display order; `draw_question` renders checkboxes, the header, wrapped question, options, custom row, and description.

## Gotchas
- An ACP agent exit cancels the open question and fails the turn. The terminal event removes only that thread's pending question; another thread's request stays. Proof: `an_acp_agent_exit_fails_the_turn_and_restarts_cleanly` and `a_terminal_event_clears_only_its_thread_pending_interactions`.
- Codex, Claude, Grok, Antigravity, and OpenCode 2 emit this event today.
- An OpenCode form field that depends on another field through `when`, an `external` field, and a hidden field are not offered, because Baton cannot evaluate or fill them. A field with no answerable content makes the whole form cancelled instead of pending, so the turn cannot hang.
- An OpenCode `multiselect` field answers with an array and every other field answers with one string, matching the form's own `Form.Value` union. A `boolean` field has no options in the schema, so the overlay offers Yes and No.
- Claude offers only the labels it sent, so its questions carry no custom row unless the option list is empty. Claude keys its answers by question text, so a prompt id is the question text, trimmed.
- The overlay shows six options at a time and keeps the selected row in the window. The Other row is always last.
- A child thread id is shortened to eight characters in the header.
- Multi-select submits the checked labels in the order the options appear, not the order they were toggled. An empty selection is not submitted.
- A secret answer is masked on screen but sent as typed, because the provider asked for the value.

## Proof

- `user_input_questions_keep_labels_custom_and_secret`
- `user_input_response_maps_native_ids_and_custom_answers`
- `question_events_do_not_leak_the_responder`
- `question_picker_answers_each_question_in_order`
- `question_picker_accepts_a_custom_answer`
- `question_picker_toggles_multi_select_options_in_display_order`
- `child_question_overlay_names_the_child_and_lists_options`
- `secret_question_masks_the_custom_answer`
- `codex_adapter_routes_user_input_questions`
- `antigravity_interactions_become_question_prompts`
- `antigravity_question_response_maps_a_unique_label_to_its_option_id`
- `antigravity_question_response_refuses_an_ambiguous_label`
- `xai_multi_select_response_keeps_every_choice`
- `acp_adapter_routes_antigravity_interactions_to_the_tui_surface`
- `question_probe.py` runs a fake Codex app-server, answers an option and a custom note through the real TUI, and checks the answers map the server received.
- `approval_probe.py` runs a fake Antigravity ACP agent, answers an approval request, then answers an `interaction_` question, and checks both native responses.
